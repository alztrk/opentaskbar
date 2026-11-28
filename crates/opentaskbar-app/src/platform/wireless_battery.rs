//! Kablosuz/Bluetooth pil veri katmanı (Rust implementation of `Services/Hardware/WirelessBattery`).
//!
//! Tek dosyada, dış bağımlılık gerektirmeyen ham Win32 FFI (setupapi.dll, hid.dll,
//! kernel32.dll, advapi32.dll) ve dinamik olarak yüklenen xinput1_4.dll kullanılır.
//! Hiçbir sağlayıcı cihazı özel olarak kilitlemez; HID I/O sınırlı overlapped
//! işlemlerle yapılır. Her sağlayıcı ayrı bir iş parçacığında çalışır ve 1.2 sn
//! içinde sonuç vermezse atlanır. 30 dakikadan eski okumalar önbellekten silinir.
//!
//! Gerçek protokoller: XInput, standart HID pil capacity/usages, Logitech HID++ 2.0,
//! Razer 90 bayt feature report, SteelSeries HID (Nova/Arctis/fare), BLE GATT
//! kayıt defteri önbelleği (WinRT yerine fallback), ayrıca vendor HID pil usages
//! (ASUS ROG, Corsair, HyperX, gamepad, ATK/VXE, 2.4G klavye). BLE GATT okuması
//! gerçek WinRT `BluetoothLEDevice` + Battery Service (0x180F / 0x2A19) üzerinden
//! yapılır; WinRT başlatılamazsa kayıt defteri önbelleğine düşülür.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::io::{Read, Write};
use std::mem::size_of;
use std::net::{TcpStream, ToSocketAddrs};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opentaskbar_core::settings::DockSettings;

// ---------------------------------------------------------------------------
// Ortak API tipleri
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BatterySource {
    XInput,
    LogitechHidPlusPlus,
    RazerHid,
    SteelSeriesHid,
    StandardHid,
    BluetoothGatt,
    AsusRogHid,
    CorsairHid,
    GamepadHid,
    HyperXHid,
    CompxAtkVxe,
    Keyboard24gHid,
    // WS-G additions (passive / loopback providers).
    Bluetooth,
    SteelSeriesGg,
    LogitechGHub,
}

/// All providers served by `run_probe`. `read_all` polls every one of them;
/// keeping the list next to the enum prevents new variants from being silently
/// left out of live discovery (they would otherwise only come from disk cache).
pub const ALL_SOURCES: [BatterySource; 15] = [
    BatterySource::SteelSeriesHid,
    BatterySource::LogitechHidPlusPlus,
    BatterySource::RazerHid,
    BatterySource::XInput,
    BatterySource::StandardHid,
    BatterySource::Bluetooth,
    BatterySource::BluetoothGatt,
    BatterySource::AsusRogHid,
    BatterySource::CorsairHid,
    BatterySource::GamepadHid,
    BatterySource::HyperXHid,
    BatterySource::CompxAtkVxe,
    BatterySource::Keyboard24gHid,
    BatterySource::LogitechGHub,
    BatterySource::SteelSeriesGg,
];

impl BatterySource {
    /// Widget etiketi (`WirelessDeviceReading.GetSourceLabel` karşılığı).
    pub fn label(self) -> &'static str {
        match self {
            Self::XInput => "XInput",
            Self::LogitechHidPlusPlus => "HID++",
            Self::RazerHid => "Razer",
            Self::SteelSeriesHid => "SteelSeries",
            Self::StandardHid => "HID",
            Self::BluetoothGatt => "BLE GATT",
            Self::AsusRogHid => "ROG",
            Self::CorsairHid => "Corsair",
            Self::GamepadHid => "Gamepad",
            Self::HyperXHid => "HyperX",
            Self::CompxAtkVxe => "ATK/VXE",
            Self::Keyboard24gHid => "2.4G",
            Self::Bluetooth => "Bluetooth",
            Self::SteelSeriesGg => "SteelSeries",
            Self::LogitechGHub => "HID++",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WirelessDeviceKind {
    Mouse,
    Keyboard,
    Headphones,
    Gamepad,
    #[default]
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WirelessReading {
    /// VID:PID:serial tabanlı, sağlayıcı önekli kararlı kimlik.
    pub id: String,
    pub name: String,
    /// 0.0 ..= 1.0 aralığında pil seviyesi.
    pub level_01: f32,
    pub is_connected: bool,
    pub is_charging: bool,
    pub source: BatterySource,
    pub stale: bool,
    pub device_kind: WirelessDeviceKind,
    /// Optional secondary / spare dock battery (e.g. SteelSeries Arctis Nova Pro / Arctis Pro).
    pub secondary_level: Option<f32>,
}

impl WirelessReading {
    pub fn level_percent(&self) -> u8 {
        (self.level_01 * 100.0).round().clamp(0.0, 100.0) as u8
    }
}

/// Reads either this port's snake_case cache key or the reference PascalCase
/// `WirelessDeviceReading` property of the same name.
fn cache_field<'a>(
    value: &'a serde_json::Value,
    snake: &str,
    pascal: &str,
) -> Option<&'a serde_json::Value> {
    value.get(snake).or_else(|| value.get(pascal))
}

use opentaskbar_core::{civil_from_days, days_from_civil};

/// Parses the `DateTimeOffset` text reference writes for `SampledAt` into Unix seconds.
/// Fractional seconds are irrelevant at the 30-minute stale threshold.
fn reference_timestamp_secs(text: &str) -> Option<u64> {
    let digits = |at: usize, len: usize| -> Option<i64> { text.get(at..at + len)?.parse().ok() };
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    let mut secs = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;
    if let Some(offset_at) = text[19..].find(['+', '-']) {
        let sign_at = 19 + offset_at;
        let offset = &text[sign_at..];
        let offset_hour = offset.get(1..3)?.parse::<i64>().ok()?;
        let offset_minute = offset.get(4..6)?.parse::<i64>().ok()?;
        // Local time = UTC + offset, therefore UTC = local - offset.
        if offset.starts_with('-') {
            secs += offset_hour * 3_600 + offset_minute * 60;
        } else {
            secs -= offset_hour * 3_600 + offset_minute * 60;
        }
    }
    u64::try_from(secs).ok()
}

fn unix_secs_to_rfc3339(secs: u64) -> String {
    let secs = secs as i64;
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let time = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// XInput yalnızca kaba seviye verir; yüzde üretmek yerine arayüz bu metni göstermelidir.
pub fn coarse_level_text(source: BatterySource, level_01: f32) -> Option<&'static str> {
    if source != BatterySource::XInput {
        return None;
    }
    match level_01 {
        x if x <= 0.25 => Some("Empty"),
        x if x <= 0.5 => Some("Low"),
        x if x <= 0.75 => Some("Medium"),
        _ => Some("Full"),
    }
}

const PROVIDER_TIMEOUT: Duration = Duration::from_millis(1200);
const STALE_WINDOW: Duration = Duration::from_secs(30 * 60);

// ---------------------------------------------------------------------------
// Win32 FFI — kernel32 / setupapi / hid / advapi32
// ---------------------------------------------------------------------------

type Handle = isize;
const INVALID_HANDLE: Handle = -1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const HID_CLASS_GUID: Guid = Guid {
    data1: 0x4D1E_55B2,
    data2: 0xF16F,
    data3: 0x11CF,
    data4: [0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30],
};

#[repr(C)]
#[derive(Clone, Copy)]
struct DevPropKey {
    fmtid: Guid,
    pid: u32,
}

#[repr(C)]
struct SpDevinfoData {
    cb_size: u32,
    class_guid: Guid,
    dev_inst: u32,
    reserved: usize,
}

/// `DEVPKEY_Bluetooth_Battery`: `{104EA319-6EE2-4701-BD47-8DDBF425BBE5}, 2`.
const DEVPKEY_BLUETOOTH_BATTERY: DevPropKey = DevPropKey {
    fmtid: Guid {
        data1: 0x104E_A319,
        data2: 0x6EE2,
        data3: 0x4701,
        data4: [0xBD, 0x47, 0x8D, 0xDB, 0xF4, 0x25, 0xBB, 0xE5],
    },
    pid: 2,
};

/// `DEVPKEY_Device_IsPresent`: `{540B947E-8B40-45BC-A8A2-6A0B894CBDA2}, 5`.
const DEVPKEY_DEVICE_IS_PRESENT: DevPropKey = DevPropKey {
    fmtid: Guid {
        data1: 0x540B_947E,
        data2: 0x8B40,
        data3: 0x45BC,
        data4: [0xA8, 0xA2, 0x6A, 0x0B, 0x89, 0x4C, 0xBD, 0xA2],
    },
    pid: 5,
};

/// `DEVPKEY_Device_FriendlyName`: `{A45C254E-DF1C-4EFD-8020-67D146A850E0}, 14`.
const DEVPKEY_DEVICE_FRIENDLY_NAME: DevPropKey = DevPropKey {
    fmtid: Guid {
        data1: 0xA45C_254E,
        data2: 0xDF1C,
        data3: 0x4EFD,
        data4: [0x80, 0x20, 0x67, 0xD1, 0x46, 0xA8, 0x50, 0xE0],
    },
    pid: 14,
};

/// `DEVPKEY_Device_DeviceDesc`: `{A45C254E-DF1C-4EFD-8020-67D146A850E0}, 2`.
const DEVPKEY_DEVICE_DESC: DevPropKey = DevPropKey {
    fmtid: Guid {
        data1: 0xA45C_254E,
        data2: 0xDF1C,
        data3: 0x4EFD,
        data4: [0x80, 0x20, 0x67, 0xD1, 0x46, 0xA8, 0x50, 0xE0],
    },
    pid: 2,
};

/// `DEVPKEY_Device_InstanceId`: `{78C34FC8-104A-4ACA-9EA4-524D52996E57}, 256`.
const DEVPKEY_DEVICE_INSTANCE_ID: DevPropKey = DevPropKey {
    fmtid: Guid {
        data1: 0x78C3_4FC8,
        data2: 0x104A,
        data3: 0x4ACA,
        data4: [0x9E, 0xA4, 0x52, 0x4D, 0x52, 0x99, 0x6E, 0x57],
    },
    pid: 256,
};

#[repr(C)]
struct SpDeviceInterfaceData {
    cb_size: u32,
    interface_class_guid: Guid,
    flags: u32,
    reserved: isize,
}

#[repr(C)]
#[derive(Default)]
struct HiddAttributes {
    size: u32,
    vendor_id: u16,
    product_id: u16,
    version_number: u16,
}

#[repr(C)]
struct HidpCaps {
    usage: u16,
    usage_page: u16,
    input_report_byte_length: u16,
    output_report_byte_length: u16,
    feature_report_byte_length: u16,
    reserved: [u16; 17],
    number_link_collection_nodes: u16,
    number_input_button_caps: u16,
    number_input_value_caps: u16,
    number_input_data_indices: u16,
    number_output_button_caps: u16,
    number_output_value_caps: u16,
    number_output_data_indices: u16,
    number_feature_button_caps: u16,
    number_feature_value_caps: u16,
    number_feature_data_indices: u16,
}

#[repr(C)]
struct HidpValueCaps {
    usage_page: u16,
    report_id: u8,
    is_alias: u8,
    bit_field: u16,
    link_collection: u16,
    link_usage: u16,
    link_usage_page: u16,
    is_range: u8,
    is_string_range: u8,
    is_designator_range: u8,
    is_absolute: u8,
    has_null: u8,
    reserved: u8,
    bit_size: u16,
    report_count: u16,
    reserved2: [u16; 5],
    units_exp: u32,
    units: u32,
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
    usage_or_usage_min: u16,
    usage_max: u16,
    string_min: u16,
    string_max: u16,
    designator_min: u16,
    designator_max: u16,
    data_index_min: u16,
    data_index_max: u16,
}

#[repr(C)]
struct Overlapped {
    internal: isize,
    internal_high: isize,
    offset: u32,
    offset_high: u32,
    h_event: Handle,
}

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const DIGCF_PRESENT: u32 = 0x2;
const DIGCF_ALLCLASSES: u32 = 0x4;
const DIGCF_DEVICEINTERFACE: u32 = 0x10;
const DEVPROP_TYPE_BYTE: u32 = 0x0000_0003;
const DEVPROP_TYPE_UINT32: u32 = 0x0000_0007;
const DEVPROP_TYPE_BOOLEAN: u32 = 0x0000_0011;
const DEVPROP_TYPE_STRING: u32 = 0x0000_0012;
const ERROR_IO_PENDING: u32 = 997;
const ERROR_NOT_FOUND: u32 = 1168;
const WAIT_OBJECT_0: u32 = 0;
const HIDP_STATUS_SUCCESS: i32 = 0x0011_0000;
const HIDP_INPUT: u32 = 0;
const HIDP_FEATURE: u32 = 2;
const HKEY_CURRENT_USER: Handle = 0x8000_0001u32 as i32 as isize;
const HKEY_LOCAL_MACHINE: Handle = 0x8000_0002u32 as i32 as isize;
const KEY_READ: u32 = 0x2_0019;
const REG_SZ: u32 = 1;
const REG_BINARY: u32 = 3;
const REG_DWORD: u32 = 4;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        lp_file_name: *const u16,
        dw_desired_access: u32,
        dw_share_mode: u32,
        lp_security_attributes: *const c_void,
        dw_creation_disposition: u32,
        dw_flags_and_attributes: u32,
        h_template_file: Handle,
    ) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn CreateEventW(
        lp_event_attributes: *const c_void,
        b_manual_reset: i32,
        b_initial_state: i32,
        lp_name: *const u16,
    ) -> Handle;
    fn WaitForSingleObject(handle: Handle, dw_milliseconds: u32) -> u32;
    fn CancelIoEx(handle: Handle, lp_overlapped: *const Overlapped) -> i32;
    fn GetOverlappedResult(
        handle: Handle,
        lp_overlapped: *const Overlapped,
        lp_transferred: *mut u32,
        b_wait: i32,
    ) -> i32;
    fn ReadFile(
        handle: Handle,
        lp_buffer: *mut u8,
        n_bytes_to_read: u32,
        lp_bytes_read: *mut u32,
        lp_overlapped: *mut Overlapped,
    ) -> i32;
    fn WriteFile(
        handle: Handle,
        lp_buffer: *const u8,
        n_bytes_to_write: u32,
        lp_bytes_written: *mut u32,
        lp_overlapped: *mut Overlapped,
    ) -> i32;
    fn GetLastError() -> u32;
    // Local kernel32 aliases for the dynamic XInput loader. These intentionally coexist with
    // the windows_sys declarations in `platform::radio` (different signatures at each call site).
    #[allow(clashing_extern_declarations)]
    fn LoadLibraryW(lp_lib_file_name: *const u16) -> Handle;
    #[allow(clashing_extern_declarations)]
    #[link_name = "GetProcAddress"]
    fn GetProcAddressWb(h_module: Handle, lp_proc_name: *const u8) -> *const c_void;
    fn FreeLibrary(h_module: Handle) -> i32;
    fn Sleep(dw_milliseconds: u32);
}

#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupDiGetClassDevsW(
        class_guid: *const Guid,
        enumerator: *const u16,
        hwnd_parent: Handle,
        flags: u32,
    ) -> Handle;
    fn SetupDiEnumDeviceInterfaces(
        info_set: Handle,
        device_info_data: *const c_void,
        interface_class_guid: *const Guid,
        member_index: u32,
        device_interface_data: *mut SpDeviceInterfaceData,
    ) -> i32;
    fn SetupDiGetDeviceInterfaceDetailW(
        info_set: Handle,
        device_interface_data: *const SpDeviceInterfaceData,
        detail_data: *mut u8,
        detail_data_size: u32,
        required_size: *mut u32,
        device_info_data: *mut c_void,
    ) -> i32;
    fn SetupDiEnumDeviceInfo(
        info_set: Handle,
        member_index: u32,
        device_info_data: *mut SpDevinfoData,
    ) -> i32;
    fn SetupDiGetDevicePropertyW(
        info_set: Handle,
        device_info_data: *const SpDevinfoData,
        property_key: *const DevPropKey,
        property_type: *mut u32,
        property_buffer: *mut u8,
        property_buffer_size: u32,
        required_size: *mut u32,
        flags: u32,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(info_set: Handle) -> i32;
}

#[link(name = "hid")]
unsafe extern "system" {
    fn HidD_GetHidGuid(hid_guid: *mut Guid);
    fn HidD_GetAttributes(handle: Handle, attributes: *mut HiddAttributes) -> i32;
    fn HidD_GetPreparsedData(handle: Handle, preparsed_data: *mut isize) -> i32;
    fn HidD_FreePreparsedData(preparsed_data: isize) -> i32;
    fn HidP_GetCaps(preparsed_data: isize, capabilities: *mut HidpCaps) -> i32;
    fn HidP_GetValueCaps(
        report_type: u32,
        value_caps: *mut HidpValueCaps,
        value_caps_length: *mut u16,
        preparsed_data: isize,
    ) -> i32;
    fn HidP_GetUsageValue(
        report_type: u32,
        usage_page: u16,
        link_collection: u16,
        usage: u16,
        usage_value: *mut u32,
        preparsed_data: isize,
        report_buffer: *const u8,
        report_length: u32,
    ) -> i32;
    fn HidD_GetProductString(handle: Handle, buffer: *mut u8, buffer_length: u32) -> i32;
    fn HidD_GetSerialNumberString(handle: Handle, buffer: *mut u8, buffer_length: u32) -> i32;
    fn HidD_GetFeature(handle: Handle, report_buffer: *mut u8, report_buffer_length: u32) -> i32;
    fn HidD_SetFeature(handle: Handle, report_buffer: *const u8, report_buffer_length: u32) -> i32;
    fn HidD_GetInputReport(
        handle: Handle,
        report_buffer: *mut u8,
        report_buffer_length: u32,
    ) -> i32;
    fn HidD_SetOutputReport(
        handle: Handle,
        report_buffer: *const u8,
        report_buffer_length: u32,
    ) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: Handle,
        sub_key: *const u16,
        options: u32,
        desired_access: u32,
        result: *mut Handle,
    ) -> i32;
    fn RegCloseKey(key: Handle) -> i32;
    fn RegEnumKeyExW(
        key: Handle,
        index: u32,
        name: *mut u16,
        name_size: *mut u32,
        reserved: *mut u32,
        class_name: *mut u16,
        class_name_size: *mut u32,
        last_write_time: *mut c_void,
    ) -> i32;
    fn RegQueryValueExW(
        key: Handle,
        value_name: *const u16,
        reserved: *mut u32,
        value_type: *mut u32,
        data: *mut u8,
        data_size: *mut u32,
    ) -> i32;
}

// ---------------------------------------------------------------------------
// Küçük yardımcılar (saf wrapper'lar)
// ---------------------------------------------------------------------------

use crate::platform::win32_helpers::wide;

fn ascii_z(text: &str) -> Vec<u8> {
    text.bytes().chain(Some(0)).collect()
}

fn invalid(handle: Handle) -> bool {
    handle == 0 || handle == INVALID_HANDLE
}

fn clamp01(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn percent_01(value: i32) -> f32 {
    clamp01(value as f32 / 100.0)
}

fn reading(
    id: String,
    name: String,
    level_01: f32,
    is_connected: bool,
    is_charging: bool,
    source: BatterySource,
) -> WirelessReading {
    let device_kind =
        super::wireless_vendors_ext::classify_wireless_device_kind(0, 0, &name, source, &id);
    WirelessReading {
        id,
        name,
        level_01: clamp01(level_01),
        is_connected,
        is_charging,
        source,
        stale: false,
        device_kind,
        secondary_level: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn reading_with_meta(
    id: String,
    name: String,
    level_01: f32,
    is_connected: bool,
    is_charging: bool,
    source: BatterySource,
    device_kind: WirelessDeviceKind,
    secondary_level: Option<f32>,
) -> WirelessReading {
    WirelessReading {
        id,
        name,
        level_01: clamp01(level_01),
        is_connected,
        is_charging,
        source,
        stale: false,
        device_kind,
        secondary_level: secondary_level.map(clamp01),
    }
}

fn utf16_from_bytes(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        let unit = u16::from_le_bytes([pair[0], pair[1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16_lossy(&units)
}

fn utf16_from_detail(bytes: &[u8]) -> String {
    if bytes.len() <= 4 {
        return String::new();
    }
    utf16_from_bytes(&bytes[4..])
}

/// Kimlik normalize etme: VID:PID:serial, serial yoksa cihaz yolunun stabilize edilmiş hash'i.
fn build_device_id(prefix: &str, vid: u16, pid: u16, serial: &str, path: &str) -> String {
    let mut clean: String = serial
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if clean.len() > 16 {
        clean = clean[clean.len() - 16..].to_owned();
    }
    if clean.is_empty() {
        clean = format!("{:08X}", fnv1a_u32(path.as_bytes()));
    }
    format!(
        "{prefix}:{vid:04X}:{pid:04X}:{}",
        clean.to_ascii_uppercase()
    )
}

fn fnv1a_u32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for &byte in bytes {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Kablolu cihaz filtresi (`IsIgnoredWiredDevice` + genel "wired" kontrolü).
pub fn is_ignored_wired_device(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return true;
    }
    if lower.contains("wraith w75") {
        return true;
    }
    lower.contains("wired") && !lower.contains("wireless")
}

fn is_ignored_wired_product(product: &str) -> bool {
    !product.trim().is_empty() && is_ignored_wired_device(product)
}

fn is_ignored_wired_reading(reading: &WirelessReading) -> bool {
    if is_ignored_wired_device(&reading.name) {
        return true;
    }
    let upper = reading.id.to_ascii_uppercase();
    upper.contains("2E3C") && upper.contains("C368")
}

fn canonical_key(reading: &WirelessReading) -> String {
    let name = normalize_name(&reading.name);
    if name.is_empty() {
        format!("id:{}", reading.id.trim().to_ascii_lowercase())
    } else {
        format!("name:{name}")
    }
}

fn source_priority(source: BatterySource) -> u8 {
    source.priority()
}

fn prefer(candidate: &WirelessReading, existing: &WirelessReading) -> bool {
    if candidate.is_connected != existing.is_connected {
        return candidate.is_connected;
    }
    if candidate.stale != existing.stale {
        return !candidate.stale;
    }
    source_priority(candidate.source) < source_priority(existing.source)
}

struct Budget {
    deadline: Instant,
}

impl Budget {
    fn new(ms: u64) -> Self {
        Self {
            deadline: Instant::now() + Duration::from_millis(ms),
        }
    }

    fn expired(&self, cancel: &AtomicBool) -> bool {
        cancel.load(Ordering::Relaxed) || Instant::now() >= self.deadline
    }
}

// ---------------------------------------------------------------------------
// HID cihaz anlık görüntüsü
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct BatteryValueCap {
    is_feature: bool,
    report_id: u8,
    usage_page: u16,
    usage: u16,
    link_collection: u16,
    logical_min: i32,
    logical_max: i32,
}

#[derive(Clone)]
struct HidDevice {
    path: String,
    vid: u16,
    pid: u16,
    usage_page: u16,
    usage: u16,
    input_len: u16,
    output_len: u16,
    feature_len: u16,
    product: String,
    serial: String,
    battery_caps: Vec<BatteryValueCap>,
}

impl HidDevice {
    fn id(&self, prefix: &str) -> String {
        build_device_id(prefix, self.vid, self.pid, &self.serial, &self.path)
    }
}

fn match_battery_usage(vc: &HidpValueCaps) -> Option<(u16, u16)> {
    let targets: &[u16] = match vc.usage_page {
        0x0006 => &[0x0020],
        0x0084 => &[0x0065, 0x00D0],
        0x0085 => &[0x0064, 0x0065, 0x0066, 0x0044, 0x0045],
        _ => &[],
    };
    for &target in targets {
        if vc.is_range == 0 {
            if vc.usage_or_usage_min == target {
                return Some((vc.usage_page, target));
            }
        } else if target >= vc.usage_or_usage_min && target <= vc.usage_max {
            return Some((vc.usage_page, target));
        }
    }
    None
}

fn extract_value_caps(
    pp: isize,
    report_type: u32,
    count: u16,
    is_feature: bool,
    out: &mut Vec<BatteryValueCap>,
) {
    if count == 0 || count > 64 {
        return;
    }
    let mut caps: Vec<HidpValueCaps> = std::iter::repeat_with(|| unsafe { std::mem::zeroed() })
        .take(count as usize)
        .collect();
    let mut length = count;
    let status = unsafe { HidP_GetValueCaps(report_type, caps.as_mut_ptr(), &mut length, pp) };
    if status != HIDP_STATUS_SUCCESS {
        return;
    }
    for cap in caps.iter().take(length as usize) {
        if let Some((usage_page, usage)) = match_battery_usage(cap) {
            out.push(BatteryValueCap {
                is_feature,
                report_id: cap.report_id,
                usage_page,
                usage,
                link_collection: cap.link_collection,
                logical_min: cap.logical_min,
                logical_max: cap.logical_max,
            });
        }
    }
}

fn read_hid_string(handle: Handle, is_serial: bool) -> String {
    let mut buffer = vec![0u8; 512];
    let ok = unsafe {
        if is_serial {
            HidD_GetSerialNumberString(handle, buffer.as_mut_ptr(), buffer.len() as u32)
        } else {
            HidD_GetProductString(handle, buffer.as_mut_ptr(), buffer.len() as u32)
        }
    };
    if ok == 0 {
        String::new()
    } else {
        utf16_from_bytes(&buffer).trim().to_owned()
    }
}

fn enumerate_hid_devices() -> Vec<HidDevice> {
    let mut guid = HID_CLASS_GUID;
    unsafe { HidD_GetHidGuid(&mut guid) };
    let set = unsafe {
        SetupDiGetClassDevsW(
            &guid,
            std::ptr::null(),
            0,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if invalid(set) {
        return Vec::new();
    }
    let mut result = Vec::new();
    for index in 0..256u32 {
        let mut interface_data: SpDeviceInterfaceData = unsafe { std::mem::zeroed() };
        interface_data.cb_size = size_of::<SpDeviceInterfaceData>() as u32;
        if unsafe {
            SetupDiEnumDeviceInterfaces(set, std::ptr::null(), &guid, index, &mut interface_data)
        } == 0
        {
            break;
        }
        let mut required = 0u32;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &interface_data,
                std::ptr::null_mut(),
                0,
                &mut required,
                std::ptr::null_mut(),
            );
        }
        if required == 0 || required > 4096 {
            continue;
        }
        let mut detail = vec![0u8; required as usize];
        let cb_size = if size_of::<isize>() == 8 { 8u32 } else { 6u32 };
        detail[..4].copy_from_slice(&cb_size.to_le_bytes());
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &interface_data,
                detail.as_mut_ptr(),
                required,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            continue;
        }
        let path = utf16_from_detail(&detail);
        if path.is_empty() {
            continue;
        }
        let path_wide = wide(&path);
        let meta = unsafe {
            OwnedHandle::new(CreateFileW(
                path_wide.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                0,
            ))
        };
        let Some(meta) = meta else { continue };
        let mut attrs = HiddAttributes {
            size: size_of::<HiddAttributes>() as u32,
            ..Default::default()
        };
        if unsafe { HidD_GetAttributes(meta.raw(), &mut attrs) } == 0 {
            continue;
        }
        let mut pp = 0isize;
        if unsafe { HidD_GetPreparsedData(meta.raw(), &mut pp) } == 0 || pp == 0 {
            continue;
        }
        let mut caps: HidpCaps = unsafe { std::mem::zeroed() };
        let mut battery_caps = Vec::new();
        if unsafe { HidP_GetCaps(pp, &mut caps) } == HIDP_STATUS_SUCCESS {
            extract_value_caps(
                pp,
                HIDP_FEATURE,
                caps.number_feature_value_caps,
                true,
                &mut battery_caps,
            );
            extract_value_caps(
                pp,
                HIDP_INPUT,
                caps.number_input_value_caps,
                false,
                &mut battery_caps,
            );
        }
        unsafe { HidD_FreePreparsedData(pp) };
        result.push(HidDevice {
            path,
            vid: attrs.vendor_id,
            pid: attrs.product_id,
            usage_page: caps.usage_page,
            usage: caps.usage,
            input_len: caps.input_report_byte_length,
            output_len: caps.output_report_byte_length,
            feature_len: caps.feature_report_byte_length,
            product: read_hid_string(meta.raw(), false),
            serial: read_hid_string(meta.raw(), true),
            battery_caps,
        });
        if result.len() >= 256 {
            break;
        }
    }
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    result
}

// ---------------------------------------------------------------------------
// Güvenli HID oturumu (sınırlı overlapped I/O + feature/output report)
// ---------------------------------------------------------------------------

struct OwnedHandle(Handle);

impl OwnedHandle {
    fn new(handle: Handle) -> Option<Self> {
        if invalid(handle) {
            None
        } else {
            Some(Self(handle))
        }
    }

    fn raw(&self) -> Handle {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct HidSession {
    handle: OwnedHandle,
}

impl HidSession {
    fn open(path: &str, feature_only: bool) -> Option<Self> {
        let access = if feature_only {
            0
        } else {
            GENERIC_READ | GENERIC_WRITE
        };
        let flags = if feature_only {
            0
        } else {
            FILE_FLAG_OVERLAPPED
        };
        let wide_path = wide(path);
        let handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                flags,
                0,
            )
        };
        let handle = OwnedHandle::new(handle)?;
        if !feature_only && is_system_mouse_or_keyboard(handle.raw()) {
            return None;
        }
        Some(Self { handle })
    }

    /// Opens an overlapped read-only HID session (`GENERIC_READ`, no write access).
    /// Used by passive input-report readers such as `GamepadHid`.
    fn open_read_only(path: &str) -> Option<Self> {
        let wide_path = wide(path);
        let handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                0,
            )
        };
        let handle = OwnedHandle::new(handle)?;
        if is_system_mouse_or_keyboard(handle.raw()) {
            return None;
        }
        Some(Self { handle })
    }

    fn get_feature(&self, buffer: &mut [u8]) -> bool {
        (unsafe { HidD_GetFeature(self.handle.raw(), buffer.as_mut_ptr(), buffer.len() as u32) })
            != 0
    }

    fn set_feature(&self, buffer: &[u8]) -> bool {
        (unsafe { HidD_SetFeature(self.handle.raw(), buffer.as_ptr(), buffer.len() as u32) }) != 0
    }

    fn get_input_report(&self, buffer: &mut [u8]) -> bool {
        (unsafe {
            HidD_GetInputReport(self.handle.raw(), buffer.as_mut_ptr(), buffer.len() as u32)
        }) != 0
    }

    fn set_output_report(&self, buffer: &[u8]) -> bool {
        (unsafe { HidD_SetOutputReport(self.handle.raw(), buffer.as_ptr(), buffer.len() as u32) })
            != 0
    }

    fn extract_usage_value(
        &self,
        is_feature: bool,
        usage_page: u16,
        link_collection: u16,
        usage: u16,
        report: &[u8],
    ) -> Option<u32> {
        let mut pp = 0isize;
        if unsafe { HidD_GetPreparsedData(self.handle.raw(), &mut pp) } == 0 || pp == 0 {
            return None;
        }
        let mut value = 0u32;
        let ok = unsafe {
            HidP_GetUsageValue(
                if is_feature { HIDP_FEATURE } else { HIDP_INPUT },
                usage_page,
                link_collection,
                usage,
                &mut value,
                pp,
                report.as_ptr(),
                report.len() as u32,
            )
        } == HIDP_STATUS_SUCCESS;
        unsafe { HidD_FreePreparsedData(pp) };
        if ok { Some(value) } else { None }
    }

    fn write_report(&self, report: &[u8], timeout_ms: u32) -> bool {
        if report.is_empty() {
            return false;
        }
        overlapped_io(self.handle.raw(), true, timeout_ms, report, None).is_some_and(|n| n > 0)
    }

    fn read_report(&self, destination: &mut [u8], timeout_ms: u32) -> Option<usize> {
        if destination.is_empty() {
            return None;
        }
        overlapped_io(self.handle.raw(), false, timeout_ms, &[], Some(destination))
    }

    /// Vendor protokolleri için write -> read turu; yanıt doğrulanana kadar en fazla 4 okuma.
    fn transact(
        &self,
        request: &[u8],
        response: &mut [u8],
        timeout_ms: u32,
        validator: impl Fn(&[u8]) -> bool,
        cancel: &AtomicBool,
    ) -> Option<usize> {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if !self.write_report(request, timeout_ms) && !self.set_output_report(request) {
            return None;
        }
        let start = Instant::now();
        for _ in 0..4 {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let elapsed = start.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            let remaining = timeout_ms.saturating_sub(elapsed);
            if remaining < 40 {
                break;
            }
            let bytes = self.read_report(response, remaining)?;
            if validator(&response[..bytes]) {
                return Some(bytes);
            }
        }
        None
    }
}

fn is_system_mouse_or_keyboard(handle: Handle) -> bool {
    let mut pp = 0isize;
    if unsafe { HidD_GetPreparsedData(handle, &mut pp) } == 0 || pp == 0 {
        return false;
    }
    let mut caps: HidpCaps = unsafe { std::mem::zeroed() };
    let status = unsafe { HidP_GetCaps(pp, &mut caps) };
    unsafe { HidD_FreePreparsedData(pp) };
    status == HIDP_STATUS_SUCCESS
        && caps.usage_page == 0x0001
        && matches!(caps.usage, 0x0002 | 0x0006)
}

/// Overlapped ReadFile/WriteFile. Zaman aşımında bekleyen IRP buffer'ı hâlâ kullanıyor
/// olabilir; bu nadir durumda buffer/event'i bilerek sızdırıyoruz (use-after-free yok).
fn overlapped_io(
    handle: Handle,
    write: bool,
    timeout_ms: u32,
    request: &[u8],
    destination: Option<&mut [u8]>,
) -> Option<usize> {
    let length = if write {
        request.len()
    } else {
        destination.as_ref().map_or(0, |d| d.len())
    };
    if length == 0 || length > u32::MAX as usize {
        return None;
    }
    let clamped = timeout_ms.clamp(50, 300);
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if invalid(event) {
        return None;
    }
    let mut overlapped: Overlapped = unsafe { std::mem::zeroed() };
    overlapped.h_event = event;
    let mut buffer = vec![0u8; length].into_boxed_slice();
    if write {
        buffer.copy_from_slice(request);
    }
    let ptr = buffer.as_mut_ptr();
    let ok = unsafe {
        if write {
            WriteFile(
                handle,
                ptr,
                length as u32,
                std::ptr::null_mut(),
                &mut overlapped,
            )
        } else {
            ReadFile(
                handle,
                ptr,
                length as u32,
                std::ptr::null_mut(),
                &mut overlapped,
            )
        }
    } != 0;
    let mut transferred = 0u32;
    let mut success = false;
    if ok {
        success = unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, 0) } != 0;
    } else if unsafe { GetLastError() } == ERROR_IO_PENDING {
        if unsafe { WaitForSingleObject(event, clamped) } == WAIT_OBJECT_0 {
            success = unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, 0) } != 0;
        } else {
            let cancelled = unsafe { CancelIoEx(handle, &overlapped) } != 0;
            let cancel_error = unsafe { GetLastError() };
            let drained = unsafe { WaitForSingleObject(event, 250) } == WAIT_OBJECT_0;
            if !drained && (!cancelled || cancel_error != ERROR_NOT_FOUND) {
                std::mem::forget(buffer);
                return None;
            }
            let _ = unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, 0) };
        }
    }
    unsafe { CloseHandle(event) };
    if !success {
        return None;
    }
    if write {
        return Some(transferred as usize);
    }
    let count = (transferred as usize).min(length);
    if let Some(destination) = destination {
        destination[..count].copy_from_slice(&buffer[..count]);
    }
    Some(count)
}

// ---------------------------------------------------------------------------
// Saf protokol yardımcıları
// ---------------------------------------------------------------------------

/// XInput kaba seviyesi -> 0.25 / 0.5 / 0.75 / 1.0 (uydurulmuş yüzde yok).
fn xinput_level_01(level: u8) -> Option<f32> {
    match level {
        0 => Some(0.25),
        1 => Some(0.50),
        2 => Some(0.75),
        3 => Some(1.00),
        _ => None,
    }
}

fn normalize_logical_to_percent(raw: i32, logical_min: i32, logical_max: i32) -> i32 {
    if logical_max > logical_min && logical_max <= 10_000 {
        if raw < logical_min || raw > logical_max {
            return -1;
        }
        let span = logical_max - logical_min;
        let percent = ((raw - logical_min) as f64 * 100.0 / span as f64).round() as i32;
        return percent.clamp(0, 100);
    }
    if (0..=100).contains(&raw) { raw } else { -1 }
}

fn direct_byte_report(
    report: &[u8],
    report_id: u8,
    logical_min: i32,
    logical_max: i32,
) -> Option<(i32, bool)> {
    if report.len() < 2 {
        return None;
    }
    if report_id != 0 && report[0] != report_id {
        return None;
    }
    let percent = normalize_logical_to_percent(i32::from(report[1]), logical_min, logical_max);
    if !(0..=100).contains(&percent) {
        return None;
    }
    let charging = report.get(2).is_some_and(|&b| b == 1);
    if percent == 0 && !charging {
        return None;
    }
    Some((percent, charging))
}

fn is_capacity_cap(cap: &BatteryValueCap) -> bool {
    (cap.usage_page == 0x0006 && cap.usage == 0x0020)
        || (cap.usage_page == 0x0084 && cap.usage == 0x0065)
        || (cap.usage_page == 0x0085 && matches!(cap.usage, 0x0064..=0x0066))
}

fn is_charging_cap(cap: &BatteryValueCap) -> bool {
    (cap.usage_page == 0x0085 && cap.usage == 0x0044)
        || (cap.usage_page == 0x0084 && cap.usage == 0x00D0)
}

// -- Logitech HID++ ---------------------------------------------------------

const HIDPP_SHORT_REPORT: u8 = 0x10;
const HIDPP_LONG_REPORT: u8 = 0x11;
const HIDPP_SOFTWARE_ID: u8 = 0x0A;
const HIDPP_FEATURE_BATTERY_STATUS: u16 = 0x1000;
const HIDPP_FEATURE_BATTERY_VOLTAGE: u16 = 0x1001;
const HIDPP_FEATURE_UNIFIED_BATTERY: u16 = 0x1004;

fn hidpp_root_request(device_index: u8, feature: u16, report_len: usize) -> Vec<u8> {
    hidpp_feature_request(
        device_index,
        0x00,
        0x00,
        &[(feature >> 8) as u8, feature as u8, 0],
        report_len,
    )
}

fn hidpp_feature_request(
    device_index: u8,
    feature_index: u8,
    function: u8,
    params: &[u8; 3],
    report_len: usize,
) -> Vec<u8> {
    let mut packet = vec![0u8; report_len];
    packet[0] = if report_len == 7 {
        HIDPP_SHORT_REPORT
    } else {
        HIDPP_LONG_REPORT
    };
    packet[1] = device_index;
    packet[2] = feature_index;
    packet[3] = ((function & 0x0F) << 4) | (HIDPP_SOFTWARE_ID & 0x0F);
    packet[4..7].copy_from_slice(params);
    packet
}

fn hidpp_is_error(reply: &[u8]) -> bool {
    reply.len() >= 4
        && matches!(reply[0], HIDPP_SHORT_REPORT | HIDPP_LONG_REPORT)
        && matches!(reply[2], 0x8F | 0xFF)
}

fn hidpp_parse_feature_index(reply: &[u8], device_index: u8) -> Option<u8> {
    if reply.len() < 5
        || !matches!(reply[0], HIDPP_SHORT_REPORT | HIDPP_LONG_REPORT)
        || reply[1] != device_index
        || reply[2] != 0
        || (reply[3] & 0x0F) != (HIDPP_SOFTWARE_ID & 0x0F)
    {
        return None;
    }
    let index = reply[4];
    if index == 0 { None } else { Some(index) }
}

fn hidpp_parse_unified(reply: &[u8], device_index: u8, feature_index: u8) -> Option<(f32, bool)> {
    if reply.len() < 7
        || !matches!(reply[0], HIDPP_SHORT_REPORT | HIDPP_LONG_REPORT)
        || reply[1] != device_index
        || reply[2] != feature_index
    {
        return None;
    }
    let soc = i32::from(reply[4]);
    let coarse = reply[5];
    let charging_status = reply[6];
    let percent = if (1..=100).contains(&soc) {
        soc
    } else if coarse != 0 {
        if coarse & 0x08 != 0 {
            90
        } else if coarse & 0x04 != 0 {
            50
        } else if coarse & 0x02 != 0 {
            20
        } else if coarse & 0x01 != 0 {
            5
        } else {
            return None;
        }
    } else if soc == 0 && (1..=4).contains(&charging_status) {
        0
    } else {
        return None;
    };
    Some((percent_01(percent), (1..=4).contains(&charging_status)))
}

fn hidpp_parse_status(reply: &[u8], device_index: u8, feature_index: u8) -> Option<(f32, bool)> {
    if reply.len() < 7
        || !matches!(reply[0], HIDPP_SHORT_REPORT | HIDPP_LONG_REPORT)
        || reply[1] != device_index
        || reply[2] != feature_index
    {
        return None;
    }
    let level = i32::from(reply[4]);
    let status = reply[6];
    let charging = (1..=4).contains(&status);
    let percent = if (1..=100).contains(&level) {
        level
    } else if level == 0 && charging {
        0
    } else {
        return None;
    };
    Some((percent_01(percent), charging))
}

fn estimate_percent_from_voltage_mv(mv: i32) -> i32 {
    if mv >= 4180 {
        return 100;
    }
    if mv <= 3500 {
        return 0;
    }
    if mv < 3700 {
        (((mv - 3500) * 20) / 200).clamp(0, 20)
    } else if mv < 3950 {
        (20 + ((mv - 3700) * 55) / 250).clamp(20, 75)
    } else {
        (75 + ((mv - 3950) * 25) / 230).clamp(75, 100)
    }
}

fn hidpp_parse_voltage(
    reply: &[u8],
    device_index: u8,
    feature_index: u8,
) -> Option<(f32, bool, i32)> {
    if reply.len() < 7
        || !matches!(reply[0], HIDPP_SHORT_REPORT | HIDPP_LONG_REPORT)
        || reply[1] != device_index
        || reply[2] != feature_index
    {
        return None;
    }
    let mv = (i32::from(reply[4]) << 8) | i32::from(reply[5]);
    if !(2800..=4500).contains(&mv) {
        return None;
    }
    Some((
        percent_01(estimate_percent_from_voltage_mv(mv)),
        reply[6] & 0x80 != 0,
        mv,
    ))
}

// -- Razer ------------------------------------------------------------------

const RAZER_PAYLOAD_LEN: usize = 90;
const RAZER_BUFFER_LEN: usize = 91;
const RAZER_STATUS_SUCCESS: u8 = 0x02;
const RAZER_COMMAND_CLASS_POWER: u8 = 0x07;
const RAZER_CMD_BATTERY: u8 = 0x80;
const RAZER_CMD_CHARGING: u8 = 0x84;

fn razer_xor_crc(payload: &[u8]) -> u8 {
    let payload = razer_normalize(payload);
    if payload.len() < RAZER_PAYLOAD_LEN {
        return 0;
    }
    let mut crc = 0u8;
    for &byte in &payload[2..=87] {
        crc ^= byte;
    }
    crc
}

fn razer_normalize(raw: &[u8]) -> &[u8] {
    if raw.len() >= RAZER_BUFFER_LEN && raw[0] == 0 && matches!(raw[1], 0x00..=0x03) {
        &raw[1..RAZER_BUFFER_LEN]
    } else if raw.len() >= RAZER_PAYLOAD_LEN {
        &raw[..RAZER_PAYLOAD_LEN]
    } else {
        raw
    }
}

fn razer_build_command(command: u8, transaction_id: u8, include_report_id: bool) -> Vec<u8> {
    let mut payload = vec![0u8; RAZER_PAYLOAD_LEN];
    payload[0] = 0x00;
    payload[1] = transaction_id;
    payload[5] = 0x02;
    payload[6] = RAZER_COMMAND_CLASS_POWER;
    payload[7] = command;
    payload[88] = razer_xor_crc(&payload);
    if !include_report_id {
        return payload;
    }
    let mut framed = vec![0u8; RAZER_BUFFER_LEN];
    framed[1..].copy_from_slice(&payload);
    framed
}

fn razer_parse_reply(raw: &[u8], expected_command: u8) -> Option<u8> {
    let payload = razer_normalize(raw);
    if payload.len() < RAZER_PAYLOAD_LEN
        || payload[0] != RAZER_STATUS_SUCCESS
        || payload[6] != RAZER_COMMAND_CLASS_POWER
        || payload[7] != expected_command
        || payload[88] != razer_xor_crc(payload)
    {
        return None;
    }
    Some(payload[9])
}

fn razer_reply_tx_id(raw: &[u8]) -> Option<u8> {
    let payload = razer_normalize(raw);
    (payload.len() >= RAZER_PAYLOAD_LEN).then_some(payload[1])
}

fn razer_level_01(raw: u8) -> f32 {
    clamp01(f32::from(raw) / 255.0)
}

// -- SteelSeries ------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SsProtocol {
    NovaPro,
    Nova7,
    Nova5,
    ArctisLegacy,
    ArctisPro,
    Mouse,
    Keyboard,
}

struct SsParsed {
    level: f32,
    charging: bool,
    connected: bool,
    spare: Option<f32>,
}

fn ss_protocol_for_pid(pid: u16) -> Option<SsProtocol> {
    match pid {
        0x12E0 | 0x12E2 | 0x12E5 | 0x12E6 => Some(SsProtocol::NovaPro),
        0x2202 | 0x2204 | 0x2206 | 0x2208 | 0x220A | 0x223A | 0x2258 | 0x225A | 0x227E | 0x2266
        | 0x226A | 0x220E | 0x2212 | 0x2216 => Some(SsProtocol::Nova7),
        0x2232 | 0x2234 | 0x2236 | 0x223E | 0x2240 | 0x2242 => Some(SsProtocol::Nova5),
        0x12B3 | 0x12B6 | 0x1260 | 0x12AD | 0x12D5 | 0x12D7 | 0x12C2 => {
            Some(SsProtocol::ArctisLegacy)
        }
        0x1290 => Some(SsProtocol::ArctisPro),
        0x1624 | 0x1626 | 0x162C | 0x162E | 0x1630 => Some(SsProtocol::Keyboard),
        0x1838 | 0x183A | 0x1852 | 0x1854 | 0x1858 | 0x185A | 0x185E | 0x1860 | 0x1842 | 0x1848
        | 0x1830 | 0x1832 | 0x172B => Some(SsProtocol::Mouse),
        _ => None,
    }
}

fn ss_default_name(pid: u16) -> &'static str {
    match pid {
        0x12E0 | 0x12E2 | 0x12E5 | 0x12E6 => "Arctis Nova Pro Wireless",
        0x2202 | 0x2204 | 0x2206 | 0x2208 | 0x220A | 0x223A | 0x2258 | 0x225A | 0x227E => {
            "Arctis Nova 7 Wireless"
        }
        0x2266 => "Arctis Nova 3P Wireless",
        0x226A => "Arctis Nova 3X Wireless",
        0x2232 | 0x2234 | 0x2236 | 0x223E | 0x2240 | 0x2242 => "Arctis Nova 5 Wireless",
        0x12B3 | 0x12B6 => "Arctis 1 Wireless",
        0x220E | 0x2212 | 0x2216 => "Arctis 7+ Wireless",
        0x1260 | 0x12AD | 0x12D5 | 0x12D7 => "Arctis 7 Wireless",
        0x12C2 => "Arctis 9 Wireless",
        0x1290 => "Arctis Pro Wireless",
        0x1624 | 0x1626 => "Apex Pro Mini Wireless",
        0x162C | 0x162E | 0x1630 => "Apex Pro TKL Wireless",
        0x1838 | 0x183A | 0x1852 | 0x1854 => "Aerox 3 Wireless",
        0x1858 | 0x185A => "Aerox 5 Wireless",
        0x185E | 0x1860 => "Aerox 9 Wireless",
        0x1842 => "Prime Wireless",
        0x1848 => "Prime Mini Wireless",
        0x1830 | 0x1832 => "Rival 3 Wireless",
        0x172B => "Rival 650 Wireless",
        _ => "SteelSeries Wireless",
    }
}

fn ss_device_kind(protocol: SsProtocol) -> WirelessDeviceKind {
    match protocol {
        SsProtocol::Mouse => WirelessDeviceKind::Mouse,
        SsProtocol::Keyboard => WirelessDeviceKind::Keyboard,
        SsProtocol::NovaPro
        | SsProtocol::Nova7
        | SsProtocol::Nova5
        | SsProtocol::ArctisLegacy
        | SsProtocol::ArctisPro => WirelessDeviceKind::Headphones,
    }
}

fn ss_normalize(raw: &[u8]) -> &[u8] {
    if raw.len() >= 4 && raw[0] == 0 && matches!(raw[1], 0x06 | 0x07 | 0x40 | 0xB0 | 0xD2) {
        &raw[1..]
    } else {
        raw
    }
}

fn ss_build_request(protocol: SsProtocol, report_len: usize, prepend_zero: bool) -> Vec<u8> {
    let length = report_len.max(64);
    let mut buffer = vec![0u8; length];
    let offset = usize::from(prepend_zero);
    match protocol {
        SsProtocol::ArctisLegacy => {
            buffer[offset] = 0x06;
            buffer[offset + 1] = 0x18;
        }
        SsProtocol::ArctisPro => {
            buffer[offset] = 0x40;
            buffer[offset + 1] = 0xAA;
        }
        SsProtocol::Mouse | SsProtocol::Keyboard => buffer[offset] = 0xD2,
        _ => {
            buffer[offset] = 0x06;
            buffer[offset + 1] = 0xB0;
        }
    }
    buffer
}

fn ss_valid_packet(protocol: SsProtocol, raw: &[u8]) -> bool {
    let payload = ss_normalize(raw);
    match protocol {
        SsProtocol::NovaPro => {
            payload.len() >= 16 && matches!(payload[0], 0x06 | 0x07) && payload[1] == 0xB0
        }
        SsProtocol::Nova7 | SsProtocol::Nova5 => {
            payload.len() >= 6 && matches!(payload[0], 0x06 | 0x07 | 0xB0)
        }
        SsProtocol::ArctisLegacy => payload.len() >= 4 && payload[0] == 0x06 && payload[1] == 0x18,
        SsProtocol::ArctisPro => payload.len() >= 4 && payload[0] == 0x40 && payload[1] == 0xAA,
        SsProtocol::Mouse | SsProtocol::Keyboard => payload.len() >= 3 && payload[0] == 0xD2,
    }
}

fn ss_scale8(step: i32) -> f32 {
    percent_01((step.clamp(0, 8) * 100 / 8).max(0))
}

fn ss_scale4(step: i32) -> f32 {
    percent_01((step.clamp(0, 4) * 100 / 4).max(0))
}

fn ss_parse(protocol: SsProtocol, raw: &[u8]) -> Option<SsParsed> {
    let reply = ss_normalize(raw);
    match protocol {
        SsProtocol::NovaPro => {
            if reply.len() < 16 || !matches!(reply[0], 0x06 | 0x07) || reply[1] != 0xB0 {
                return None;
            }
            let headset = i32::from(reply[6]);
            let spare = reply[7];
            let status = reply[15];
            if headset > 8 || !matches!(status, 0x01 | 0x02 | 0x08) {
                return None;
            }
            Some(SsParsed {
                level: ss_scale8(headset),
                charging: status == 0x02,
                connected: matches!(status, 0x02 | 0x08),
                spare: if spare <= 8 {
                    Some(ss_scale8(i32::from(spare)))
                } else {
                    None
                },
            })
        }
        SsProtocol::Nova7 => {
            if reply.len() < 4 {
                return None;
            }
            let offset = if matches!(reply[0], 0x06 | 0x07) && reply[1] == 0xB0 {
                2
            } else if reply[0] == 0xB0 {
                1
            } else {
                return None;
            };
            if reply.len() < offset + 2 {
                return None;
            }
            let step = i32::from(reply[offset]);
            let state = reply[offset + 1];
            if step > 4 || state > 3 {
                return None;
            }
            Some(SsParsed {
                level: ss_scale4(step),
                charging: state == 0x01,
                connected: state != 0,
                spare: None,
            })
        }
        SsProtocol::Nova5 => {
            if reply.len() < 5 || !matches!(reply[0], 0x06 | 0x07) || reply[1] != 0xB0 {
                return None;
            }
            let status = reply[2];
            let level = i32::from(reply[3]);
            if level > 100 || status > 3 {
                return None;
            }
            Some(SsParsed {
                level: percent_01(level),
                charging: status == 0x01,
                connected: status != 0,
                spare: None,
            })
        }
        SsProtocol::ArctisLegacy => {
            if reply.len() < 4 || reply[0] != 0x06 || reply[1] != 0x18 {
                return None;
            }
            let level = i32::from(reply[2]);
            if level == 0 || level > 100 {
                return None;
            }
            Some(SsParsed {
                level: percent_01(level),
                charging: reply[3] == 0x01,
                connected: true,
                spare: None,
            })
        }
        SsProtocol::ArctisPro => {
            if reply.len() < 4 || reply[0] != 0x40 || reply[1] != 0xAA {
                return None;
            }
            let headset = i32::from(reply[2]);
            if headset > 4 {
                return None;
            }
            let spare = reply[3];
            Some(SsParsed {
                level: ss_scale4(headset),
                charging: false,
                connected: headset > 0,
                spare: if spare <= 4 {
                    Some(ss_scale4(i32::from(spare)))
                } else {
                    None
                },
            })
        }
        SsProtocol::Mouse | SsProtocol::Keyboard => {
            if reply.len() < 2 || reply[0] != 0xD2 {
                return None;
            }
            let raw = reply[1];
            let level = i32::from(raw & 0x7F);
            if level == 0 || level > 100 {
                return None;
            }
            Some(SsParsed {
                level: percent_01(level),
                charging: raw & 0x80 != 0,
                connected: true,
                spare: None,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Sağlayıcı 1: XInput (xinput1_4.dll, dinamik yükleme)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Default)]
struct XInputGamepad {
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
    thumb_lx: i16,
    thumb_ly: i16,
    thumb_rx: i16,
    thumb_ry: i16,
}

#[repr(C)]
#[derive(Default)]
struct XInputState {
    packet_number: u32,
    gamepad: XInputGamepad,
}

#[repr(C)]
#[derive(Default)]
struct XInputBatteryInformation {
    battery_type: u8,
    battery_level: u8,
}

type XInputGetStateFn = unsafe extern "system" fn(u32, *mut XInputState) -> u32;
type XInputGetBatteryFn = unsafe extern "system" fn(u32, u8, *mut XInputBatteryInformation) -> u32;

fn get_proc(module: Handle, name: &str) -> Option<*const c_void> {
    let symbol = ascii_z(name);
    let address = unsafe { GetProcAddressWb(module, symbol.as_ptr()) };
    if address.is_null() {
        None
    } else {
        Some(address)
    }
}

fn probe_xinput(cancel: &AtomicBool) -> Vec<WirelessReading> {
    let mut module = unsafe { LoadLibraryW(wide("xinput1_4.dll").as_ptr()) };
    if invalid(module) {
        module = unsafe { LoadLibraryW(wide("xinput1_3.dll").as_ptr()) };
    }
    if invalid(module) {
        return Vec::new();
    }
    let (Some(state_proc), Some(battery_proc)) = (
        get_proc(module, "XInputGetState"),
        get_proc(module, "XInputGetBatteryInformation"),
    ) else {
        unsafe { FreeLibrary(module) };
        return Vec::new();
    };
    let get_state: XInputGetStateFn = unsafe { std::mem::transmute_copy(&state_proc) };
    let get_battery: XInputGetBatteryFn = unsafe { std::mem::transmute_copy(&battery_proc) };
    let mut result = Vec::new();
    for slot in 0..4u32 {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut state = XInputState::default();
        if unsafe { get_state(slot, &mut state) } != 0 {
            continue;
        }
        let mut info = XInputBatteryInformation::default();
        if unsafe { get_battery(slot, 0, &mut info) } != 0 {
            continue;
        }
        // 0x02 = alkaline, 0x03 = NiMH; kablü (0x01) ve bilinmeyen (0xFF) atlanır.
        if !matches!(info.battery_type, 0x02 | 0x03) {
            continue;
        }
        let Some(level) = xinput_level_01(info.battery_level) else {
            continue;
        };
        result.push(reading(
            format!("xinput:slot:{slot}"),
            format!("Xbox Wireless Controller ({})", slot + 1),
            level,
            true,
            false,
            BatterySource::XInput,
        ));
    }
    unsafe { FreeLibrary(module) };
    result
}

// ---------------------------------------------------------------------------
// Sağlayıcı 2: standart HID pil capacity (yalnızca okuma)
// ---------------------------------------------------------------------------

fn read_hid_capacity(
    device: &HidDevice,
    session: &HidSession,
    cap: &BatteryValueCap,
) -> Option<(f32, bool)> {
    let report_len = if cap.is_feature {
        device.feature_len.max(2)
    } else {
        device.input_len.max(2)
    };
    if !(2..=512).contains(&report_len) {
        return None;
    }
    let mut buffer = vec![0u8; report_len as usize];
    buffer[0] = cap.report_id;
    let read_ok = if cap.is_feature {
        session.get_feature(&mut buffer)
    } else {
        session.get_input_report(&mut buffer)
    };
    if !read_ok {
        return None;
    }
    let raw = session
        .extract_usage_value(
            cap.is_feature,
            cap.usage_page,
            cap.link_collection,
            cap.usage,
            &buffer,
        )
        .and_then(|value| {
            let percent =
                normalize_logical_to_percent(value as i32, cap.logical_min, cap.logical_max);
            ((0..=100).contains(&percent)).then_some((percent, false))
        })
        .or_else(|| direct_byte_report(&buffer, cap.report_id, cap.logical_min, cap.logical_max))?;
    let mut charging = raw.1;
    if let Some(charge_cap) = device.battery_caps.iter().find(|c| {
        is_charging_cap(c) && c.is_feature == cap.is_feature && c.report_id == cap.report_id
    }) && let Some(value) = session.extract_usage_value(
        charge_cap.is_feature,
        charge_cap.usage_page,
        charge_cap.link_collection,
        charge_cap.usage,
        &buffer,
    ) {
        charging = value != 0;
    }
    Some((percent_01(raw.0), charging))
}

fn probe_standard_hid(
    devices: &[HidDevice],
    cancel: &AtomicBool,
    source: BatterySource,
    filter: impl Fn(&HidDevice) -> bool,
) -> Vec<WirelessReading> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for device in devices {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !filter(device) || is_ignored_wired_product(&device.product) {
            continue;
        }
        if seen.contains(&(device.vid, device.pid)) {
            continue;
        }
        let Some(cap) = device.battery_caps.iter().find(|c| is_capacity_cap(c)) else {
            continue;
        };
        let Some(session) = HidSession::open(&device.path, true) else {
            continue;
        };
        let Some((level, charging)) = read_hid_capacity(device, &session, cap) else {
            continue;
        };
        if level <= 0.0 {
            continue;
        }
        let name = if device.product.trim().is_empty() {
            format!("HID Wireless ({:04X}:{:04X})", device.vid, device.pid)
        } else {
            device.product.trim().to_owned()
        };
        let prefix = match source {
            BatterySource::AsusRogHid => "rog",
            BatterySource::CorsairHid => "corsair",
            BatterySource::HyperXHid => "hyperx",
            BatterySource::GamepadHid => "gamepad",
            BatterySource::CompxAtkVxe => "compx",
            BatterySource::Keyboard24gHid => "keyboard24g",
            _ => "hid",
        };
        let id = device.id(prefix);
        let kind = super::wireless_vendors_ext::classify_wireless_device_kind(
            device.vid, device.pid, &name, source, &id,
        );
        seen.insert((device.vid, device.pid));
        result.push(reading_with_meta(
            id, name, level, true, charging, source, kind, None,
        ));
    }
    result
}

fn is_custom_protocol_device(device: &HidDevice) -> bool {
    matches!(device.vid, 0x046D | 0x1038 | 0x1532)
}

// ---------------------------------------------------------------------------
// Sağlayıcı 3: Logitech HID++ 2.0
// ---------------------------------------------------------------------------

fn hidpp_matches(device: &HidDevice) -> bool {
    device.vid == 0x046D
        && device.usage_page == 0xFF00
        && (matches!(device.input_len, 7 | 20) || matches!(device.output_len, 7 | 20))
}

fn hidpp_transact(
    session: &HidSession,
    request: &[u8],
    response: &mut [u8],
    device_index: u8,
    expected_feature: u8,
    _budget: &Budget,
    cancel: &AtomicBool,
) -> Option<usize> {
    let validator = |packet: &[u8]| {
        packet.len() >= 4
            && matches!(packet[0], HIDPP_SHORT_REPORT | HIDPP_LONG_REPORT)
            && packet[1] == device_index
            && (packet[2] == expected_feature || hidpp_is_error(packet))
    };
    let len = session.transact(request, response, 180, validator, cancel)?;
    if hidpp_is_error(&response[..len]) {
        None
    } else {
        Some(len)
    }
}

fn hidpp_resolve_feature(
    session: &HidSession,
    device_index: u8,
    feature: u16,
    report_len: usize,
    budget: &Budget,
    cancel: &AtomicBool,
) -> Option<u8> {
    let request = hidpp_root_request(device_index, feature, report_len);
    let mut response = vec![0u8; report_len.max(20)];
    let len = hidpp_transact(
        session,
        &request,
        &mut response,
        device_index,
        0x00,
        budget,
        cancel,
    )?;
    hidpp_parse_feature_index(&response[..len], device_index)
}

#[allow(clippy::too_many_arguments)]
fn hidpp_query_feature<T>(
    session: &HidSession,
    device_index: u8,
    feature: u16,
    function: u8,
    report_len: usize,
    response_len: usize,
    budget: &Budget,
    cancel: &AtomicBool,
    parse: fn(&[u8], u8, u8) -> Option<T>,
) -> Option<T> {
    let index = hidpp_resolve_feature(session, device_index, feature, report_len, budget, cancel)?;
    let request = hidpp_feature_request(device_index, index, function, &[0, 0, 0], report_len);
    let mut response = vec![0u8; response_len];
    let len = hidpp_transact(
        session,
        &request,
        &mut response,
        device_index,
        index,
        budget,
        cancel,
    )?;
    parse(&response[..len], device_index, index)
}

fn probe_logitech(devices: &[HidDevice], cancel: &AtomicBool) -> Vec<WirelessReading> {
    let budget = Budget::new(850);
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for device in devices.iter().filter(|d| hidpp_matches(d)) {
        if budget.expired(cancel) {
            break;
        }
        let report_len = if device.output_len == 20 || device.input_len == 20 {
            20
        } else {
            7
        };
        let Some(session) = HidSession::open(&device.path, false) else {
            continue;
        };
        for device_index in [0xFFu8, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06] {
            if budget.expired(cancel) {
                break;
            }
            if !seen.insert((device.pid, device_index)) {
                continue;
            }
            let response_len = device.input_len.max(report_len as u16) as usize;
            let level = hidpp_query_feature(
                &session,
                device_index,
                HIDPP_FEATURE_UNIFIED_BATTERY,
                0x01,
                report_len,
                response_len,
                &budget,
                cancel,
                hidpp_parse_unified,
            )
            .or_else(|| {
                hidpp_query_feature(
                    &session,
                    device_index,
                    HIDPP_FEATURE_BATTERY_STATUS,
                    0x00,
                    report_len,
                    response_len,
                    &budget,
                    cancel,
                    hidpp_parse_status,
                )
            })
            .or_else(|| {
                hidpp_query_feature(
                    &session,
                    device_index,
                    HIDPP_FEATURE_BATTERY_VOLTAGE,
                    0x00,
                    report_len,
                    response_len,
                    &budget,
                    cancel,
                    hidpp_parse_voltage,
                )
                .map(|(percent, is_charging, _)| (percent, is_charging))
            });
            if let Some((percent, is_charging)) = level {
                let name = if device.product.trim().is_empty() {
                    "Logitech Wireless".to_owned()
                } else {
                    device.product.trim().to_owned()
                };
                let id = format!("{}:{device_index:02X}", device.id("logitech"));
                let kind = super::wireless_vendors_ext::classify_wireless_device_kind(
                    device.vid,
                    device.pid,
                    &name,
                    BatterySource::LogitechHidPlusPlus,
                    &id,
                );
                result.push(reading_with_meta(
                    id,
                    name,
                    percent,
                    true,
                    is_charging,
                    BatterySource::LogitechHidPlusPlus,
                    kind,
                    None,
                ));
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Sağlayıcı 4: Razer 90 bayt feature report
// ---------------------------------------------------------------------------

fn razer_query(
    session: &HidSession,
    command: u8,
    transaction_id: u8,
    buffer_len: usize,
) -> Option<u8> {
    let mut request = razer_build_command(command, transaction_id, buffer_len >= RAZER_BUFFER_LEN);
    request.resize(buffer_len, 0);
    if !session.set_feature(&request) {
        return None;
    }
    unsafe { Sleep(15) };
    let mut response = vec![0u8; buffer_len];
    if !session.get_feature(&mut response) {
        return None;
    }
    razer_parse_reply(&response, command)
}

fn probe_razer(devices: &[HidDevice], cancel: &AtomicBool) -> Vec<WirelessReading> {
    let mut result = Vec::new();
    let mut seen: HashSet<(String, u8)> = HashSet::new();
    for device in devices
        .iter()
        .filter(|d| d.vid == 0x1532 && d.feature_len >= 90)
    {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(session) = HidSession::open(&device.path, true) else {
            continue;
        };
        let buffer_len = (device.feature_len as usize).max(RAZER_BUFFER_LEN);
        let mut path_echoed_tx: HashSet<u8> = HashSet::new();
        let mut path_signatures: HashSet<(u8, bool)> = HashSet::new();
        for transaction_id in [0x1Fu8, 0x3F, 0xFF, 0x7F, 0x00] {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if !seen.insert((device.path.clone(), transaction_id)) {
                continue;
            }
            let mut request = razer_build_command(
                RAZER_CMD_BATTERY,
                transaction_id,
                buffer_len >= RAZER_BUFFER_LEN,
            );
            request.resize(buffer_len, 0);
            if !session.set_feature(&request) {
                continue;
            }
            unsafe { Sleep(15) };
            let mut response = vec![0u8; buffer_len];
            if !session.get_feature(&mut response) {
                continue;
            }
            let Some(raw_level) = razer_parse_reply(&response, RAZER_CMD_BATTERY) else {
                continue;
            };
            if raw_level == 0 {
                continue;
            }
            let echoed_tx = razer_reply_tx_id(&response).unwrap_or(transaction_id);
            if !path_echoed_tx.insert(echoed_tx) {
                continue;
            }
            let charging = razer_query(&session, RAZER_CMD_CHARGING, transaction_id, buffer_len)
                .is_some_and(|value| value != 0);
            // Single-device dongles often echo the same level across arbitrary tx_ids;
            // only keep additional tx_ids on the same path when the dongle echoes a distinct slot.
            if !path_signatures.insert((raw_level, charging)) && echoed_tx != transaction_id {
                continue;
            }
            let name = if device.product.trim().is_empty() {
                "Razer Wireless".to_owned()
            } else {
                device.product.trim().to_owned()
            };
            let id = format!(
                "{}:{transaction_id:02X}:{:08X}",
                device.id("razer"),
                fnv1a_u32(device.path.as_bytes())
            );
            let kind = super::wireless_vendors_ext::classify_wireless_device_kind(
                device.vid,
                device.pid,
                &name,
                BatterySource::RazerHid,
                &id,
            );
            result.push(reading_with_meta(
                id,
                name,
                razer_level_01(raw_level),
                true,
                charging,
                BatterySource::RazerHid,
                kind,
                None,
            ));
            // On a single-interface path, if the device didn't preserve our tx_id (non-HyperSpeed),
            // stop probing further tx_ids on this path.
            if echoed_tx != transaction_id {
                break;
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Sağlayıcı 5: SteelSeries HID
// ---------------------------------------------------------------------------

fn probe_steelseries(devices: &[HidDevice], cancel: &AtomicBool) -> Vec<WirelessReading> {
    let budget = Budget::new(1000);
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for device in devices {
        if budget.expired(cancel) {
            break;
        }
        if device.vid != 0x1038
            || !matches!(device.usage_page, 0xFFC0 | 0xFF00 | 0xFF02)
            || !matches!(device.input_len, 64 | 65)
            || !matches!(device.output_len, 64 | 65)
        {
            continue;
        }
        let Some(protocol) = ss_protocol_for_pid(device.pid) else {
            continue;
        };
        if seen.contains(&(device.vid, device.pid)) {
            continue;
        }
        let Some(session) = HidSession::open(&device.path, false) else {
            continue;
        };
        let out_len = (device.output_len.max(64)) as usize;
        let in_len = (device.input_len.max(64)) as usize;
        let try_query = |req_len: usize, prepend_zero: bool| {
            let request = ss_build_request(protocol, req_len, prepend_zero);
            let mut response = vec![0u8; in_len];
            session
                .transact(
                    &request,
                    &mut response,
                    250,
                    |packet| ss_valid_packet(protocol, packet),
                    cancel,
                )
                .and_then(|len| ss_parse(protocol, &response[..len]))
        };
        let parsed = try_query(out_len, out_len == 65)
            .or_else(|| (out_len == 64).then(|| try_query(65, true)).flatten());
        let Some(parsed) = parsed else { continue };
        let name = if device.product.trim().is_empty() {
            ss_default_name(device.pid).to_owned()
        } else {
            device.product.trim().to_owned()
        };
        if !parsed.connected {
            continue;
        }
        seen.insert((device.vid, device.pid));
        result.push(reading_with_meta(
            device.id("steelseries"),
            name,
            parsed.level,
            parsed.connected,
            parsed.charging,
            BatterySource::SteelSeriesHid,
            ss_device_kind(protocol),
            parsed.spare,
        ));
    }
    result
}

// ---------------------------------------------------------------------------
// Sağlayıcı 6: BLE GATT (WinRT yerine kayıt defteri önbelleği fallback'i)
// ---------------------------------------------------------------------------

struct RegKey(Handle);

impl RegKey {
    fn open(root: Handle, path: &str) -> Option<Self> {
        let mut key: Handle = 0;
        let status = unsafe { RegOpenKeyExW(root, wide(path).as_ptr(), 0, KEY_READ, &mut key) };
        if status == 0 && !invalid(key) {
            Some(Self(key))
        } else {
            None
        }
    }

    fn raw(&self) -> Handle {
        self.0
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn reg_query_raw(key: Handle, name: &str, buffer: &mut [u8]) -> Option<(u32, usize)> {
    let mut size = buffer.len() as u32;
    let mut kind = 0u32;
    // SAFETY: owned buffer outlives the call; name is NUL-terminated.
    let status = unsafe {
        RegQueryValueExW(
            key,
            wide(name).as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            buffer.as_mut_ptr(),
            &mut size,
        )
    };
    (status == 0).then_some((kind, (size as usize).min(buffer.len())))
}

fn reg_dword(key: Handle, name: &str) -> Option<u32> {
    let mut buf = [0u8; 4];
    let (kind, size) = reg_query_raw(key, name, &mut buf)?;
    (kind == REG_DWORD && size >= 4).then(|| u32::from_le_bytes(buf))
}

fn reg_string(key: Handle, name: &str) -> Option<String> {
    let mut buffer = [0u8; 1024];
    let (kind, size) = reg_query_raw(key, name, &mut buffer)?;
    if kind != REG_SZ {
        return None;
    }
    let text = utf16_from_bytes(&buffer[..size]);
    (!text.is_empty()).then_some(text)
}

fn reg_subkeys(key: Handle) -> Vec<String> {
    let mut names = Vec::new();
    for index in 0..128u32 {
        let mut buffer = vec![0u16; 256];
        let mut size = buffer.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                buffer.as_mut_ptr(),
                &mut size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            break;
        }
        let end = (size as usize).min(buffer.len());
        names.push(String::from_utf16_lossy(&buffer[..end]));
    }
    names
}

fn ble_registry_reading(key: Handle, id_hint: &str) -> Option<WirelessReading> {
    let percent = [
        "BatteryLevel",
        "BatteryPercent",
        "BatteryPercentage",
        "Percentage",
    ]
    .iter()
    .find_map(|name| reg_dword(key, name))?;
    if percent > 100 {
        return None;
    }
    let name = ["FriendlyName", "Name", "DisplayName"]
        .iter()
        .find_map(|name| reg_string(key, name))
        .unwrap_or_else(|| format!("BLE Device ({id_hint})"));
    if is_ignored_wired_device(&name) {
        return None;
    }
    let id = format!("ble:{:08X}", fnv1a_u32(id_hint.as_bytes()));
    Some(reading(
        id,
        name,
        percent_01(percent as i32),
        true,
        false,
        BatterySource::BluetoothGatt,
    ))
}

/// Reads the BLE Battery Service (0x180F) characteristic (0x2A19) through WinRT.
/// Returns `None` when WinRT/Bluetooth initialization fails (so callers may fall back
/// to cached registry data), or `Some(readings)` when enumeration succeeded cleanly.
fn ble_gatt_readings(cancel: &AtomicBool) -> Option<Vec<WirelessReading>> {
    use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothLEDevice};
    use windows::Devices::Enumeration::DeviceInformation;
    use windows::Storage::Streams::DataReader;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    use windows::core::GUID;

    const BATTERY_SERVICE: GUID = GUID::from_u128(0x0000180f_0000_1000_8000_00805f9b34fb);
    const BATTERY_LEVEL: GUID = GUID::from_u128(0x00002a19_0000_1000_8000_00805f9b34fb);

    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() && hr.0 != 0x8001_0106u32 as i32 {
        return None;
    }
    let initialized = hr.is_ok();

    let result = (|| -> windows::core::Result<Vec<WirelessReading>> {
        let selector = BluetoothLEDevice::GetDeviceSelector()?;
        let devices = DeviceInformation::FindAllAsyncAqsFilter(&selector)?.join()?;
        let mut readings = Vec::new();
        for info in devices {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let device_id = info.Id()?;
            let name = info.Name()?.to_string_lossy();
            let read = (|| -> windows::core::Result<Option<u8>> {
                let device = BluetoothLEDevice::FromIdAsync(&device_id)?.join()?;
                if device.ConnectionStatus()? != BluetoothConnectionStatus::Connected {
                    return Ok(None);
                }
                let services = device.GetGattServicesAsync()?.join()?;
                for service in services.Services()? {
                    if service.Uuid()? != BATTERY_SERVICE {
                        continue;
                    }
                    let characteristics = service.GetCharacteristicsAsync()?.join()?;
                    for characteristic in characteristics.Characteristics()? {
                        if characteristic.Uuid()? != BATTERY_LEVEL {
                            continue;
                        }
                        let value = characteristic.ReadValueAsync()?.join()?;
                        let buffer = value.Value()?;
                        let reader = DataReader::FromBuffer(&buffer)?;
                        return Ok(Some(reader.ReadByte()?));
                    }
                }
                Ok(None)
            })();
            if let Ok(Some(percent)) = read
                && percent <= 100
                && !is_ignored_wired_device(&name)
            {
                let dev_id_str = device_id.to_string_lossy();
                let id = format!("ble-gatt:{:08X}", fnv1a_u32(dev_id_str.as_bytes()));
                let kind = super::wireless_vendors_ext::classify_wireless_device_kind(
                    0,
                    0,
                    &name,
                    BatterySource::BluetoothGatt,
                    &dev_id_str,
                );
                readings.push(reading_with_meta(
                    id,
                    name,
                    percent_01(percent as i32),
                    true,
                    false,
                    BatterySource::BluetoothGatt,
                    kind,
                    None,
                ));
            }
        }
        Ok(readings)
    })();
    if initialized {
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
    result.ok()
}

fn probe_ble_gatt(_devices: &[HidDevice], cancel: &AtomicBool) -> Vec<WirelessReading> {
    if let Some(readings) = ble_gatt_readings(cancel) {
        return readings;
    }
    let mut result = Vec::new();
    let roots = [
        (
            HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Bluetooth\Devices",
        ),
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Services\BTHPORT\Parameters\Devices",
        ),
    ];
    for (root, path) in roots {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(key) = RegKey::open(root, path) else {
            continue;
        };
        for subkey in reg_subkeys(key.raw()) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if let Some(child) = RegKey::open(key.raw(), &subkey)
                && let Some(item) = ble_registry_reading(child.raw(), &subkey)
            {
                result.push(item);
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// WS-4: Bluetooth PnP reader + G-Hub WebSocket / SteelSeries coreProps loopback
// ---------------------------------------------------------------------------

/// Accepts only an explicitly loopback WebSocket/HTTP endpoint for the given port.
fn parse_loopback_endpoint(uri: &str, expected_port: u16, allow_websocket: bool) -> Option<String> {
    let lower = uri.to_ascii_lowercase();
    let scheme_end = lower.find("://")?;
    let scheme = &lower[..scheme_end];
    if !(scheme == "http" || (allow_websocket && scheme == "ws")) {
        return None;
    }
    let rest = &lower[scheme_end + 3..];
    let authority = rest.split('/').next()?.split('?').next()?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()?),
        None => (authority, 80),
    };
    if port != expected_port || !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return None;
    }
    Some(format!("{host}:{port}"))
}

/// Reads a 0..100 battery value from a PnP registry value (DWORD, one-byte
/// BLOB or decimal string) without assuming a fixed value type.
fn reg_percent(key: Handle, name: &str) -> Option<i32> {
    let mut buffer = [0u8; 64];
    let (kind, size) = reg_query_raw(key, name, &mut buffer)?;
    let value = match kind {
        REG_DWORD if size >= 4 => u32::from_le_bytes(buffer[..4].try_into().ok()?) as i32,
        REG_BINARY if size >= 1 => buffer[0] as i32,
        REG_SZ => utf16_from_bytes(&buffer[..size])
            .trim()
            .parse::<i32>()
            .ok()?,
        _ => return None,
    };
    (0..=100).contains(&value).then_some(value)
}

fn setupdi_prop_string(
    info_set: Handle,
    devinfo: &SpDevinfoData,
    key: &DevPropKey,
) -> Option<String> {
    let mut prop_type = 0u32;
    let mut required = 0u32;
    let mut buffer = [0u8; 512];
    let ok = unsafe {
        SetupDiGetDevicePropertyW(
            info_set,
            devinfo,
            key,
            &mut prop_type,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut required,
            0,
        )
    } != 0;
    if !ok || prop_type != DEVPROP_TYPE_STRING {
        return None;
    }
    let len = (required as usize).min(buffer.len());
    let text = utf16_from_bytes(&buffer[..len]).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn setupdi_prop_is_present(info_set: Handle, devinfo: &SpDevinfoData) -> Option<bool> {
    let mut prop_type = 0u32;
    let mut required = 0u32;
    let mut buffer = [0u8; 4];
    let ok = unsafe {
        SetupDiGetDevicePropertyW(
            info_set,
            devinfo,
            &DEVPKEY_DEVICE_IS_PRESENT,
            &mut prop_type,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut required,
            0,
        )
    } != 0;
    if ok && prop_type == DEVPROP_TYPE_BOOLEAN && required >= 1 {
        Some(buffer[0] != 0)
    } else {
        None
    }
}

fn setupdi_prop_bluetooth_battery(info_set: Handle, devinfo: &SpDevinfoData) -> Option<i32> {
    let mut prop_type = 0u32;
    let mut required = 0u32;
    let mut buffer = [0u8; 8];
    let ok = unsafe {
        SetupDiGetDevicePropertyW(
            info_set,
            devinfo,
            &DEVPKEY_BLUETOOTH_BATTERY,
            &mut prop_type,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut required,
            0,
        )
    } != 0;
    if !ok {
        return None;
    }
    let value = match prop_type {
        DEVPROP_TYPE_BYTE if required >= 1 => i32::from(buffer[0]),
        DEVPROP_TYPE_UINT32 if required >= 4 => {
            u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as i32
        }
        _ => return None,
    };
    (0..=100).contains(&value).then_some(value)
}

/// Collects present Bluetooth PnP instance IDs (`BTHENUM`, `BTHLE`, `BLUETOOTH`)
/// via SetupDi so registry fallback never reports ghost offline devices.
fn collect_present_bluetooth_instances(cancel: &AtomicBool) -> Option<HashSet<String>> {
    let mut present = HashSet::new();
    let mut any_opened = false;
    for enumerator in ["BTHENUM", "BTHLE", "BLUETOOTH"] {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let enum_wide = wide(enumerator);
        let set = unsafe {
            SetupDiGetClassDevsW(
                std::ptr::null(),
                enum_wide.as_ptr(),
                0,
                DIGCF_ALLCLASSES | DIGCF_PRESENT,
            )
        };
        if invalid(set) {
            continue;
        }
        any_opened = true;
        for index in 0..512u32 {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let mut devinfo: SpDevinfoData = unsafe { std::mem::zeroed() };
            devinfo.cb_size = size_of::<SpDevinfoData>() as u32;
            if unsafe { SetupDiEnumDeviceInfo(set, index, &mut devinfo) } == 0 {
                break;
            }
            if setupdi_prop_is_present(set, &devinfo) == Some(false) {
                continue;
            }
            if let Some(instance_id) =
                setupdi_prop_string(set, &devinfo, &DEVPKEY_DEVICE_INSTANCE_ID)
            {
                present.insert(instance_id.to_ascii_uppercase());
            }
        }
        unsafe { SetupDiDestroyDeviceInfoList(set) };
    }
    any_opened.then_some(present)
}

/// Reads live battery percentages from present Bluetooth PnP devices via native
/// Win32 SetupDi (`DEVPKEY_Bluetooth_Battery` + `DEVPKEY_Device_IsPresent`) —
/// capturing AirPods/Beats, Sony WH/WF-1000XM, Bose, Sennheiser, Jabra, Anker
/// Soundcore, Galaxy Buds, and standard HFP/BLE peripherals with zero PowerShell
/// spawns and zero ghost offline devices. Falls back to PnP registry ONLY for
/// instances confirmed present by SetupDi.
fn bluetooth_pnp_readings(cancel: &AtomicBool) -> Vec<WirelessReading> {
    let mut seen = HashSet::new();
    let mut readings = Vec::new();

    // Primary path: SetupDiGetClassDevsW(DIGCF_ALLCLASSES | DIGCF_PRESENT) + DEVPKEY_Bluetooth_Battery.
    let set = unsafe {
        SetupDiGetClassDevsW(
            std::ptr::null(),
            std::ptr::null(),
            0,
            DIGCF_ALLCLASSES | DIGCF_PRESENT,
        )
    };
    if !invalid(set) {
        for index in 0..2048u32 {
            if cancel.load(Ordering::Relaxed) || readings.len() >= 32 {
                break;
            }
            let mut devinfo: SpDevinfoData = unsafe { std::mem::zeroed() };
            devinfo.cb_size = size_of::<SpDevinfoData>() as u32;
            if unsafe { SetupDiEnumDeviceInfo(set, index, &mut devinfo) } == 0 {
                break;
            }
            let Some(level) = setupdi_prop_bluetooth_battery(set, &devinfo) else {
                continue;
            };
            if setupdi_prop_is_present(set, &devinfo) == Some(false) {
                continue;
            }
            let instance_id = setupdi_prop_string(set, &devinfo, &DEVPKEY_DEVICE_INSTANCE_ID)
                .unwrap_or_else(|| format!("setupdi:{index}"));
            let raw_name = setupdi_prop_string(set, &devinfo, &DEVPKEY_DEVICE_FRIENDLY_NAME)
                .or_else(|| setupdi_prop_string(set, &devinfo, &DEVPKEY_DEVICE_DESC))
                .unwrap_or_else(|| normalize_name(&instance_id));
            let name = super::wireless_vendors_ext::clean_bluetooth_device_name(&raw_name);
            if is_ignored_wired_device(&name) {
                continue;
            }
            let id = format!(
                "bt-pnp:{:08X}",
                fnv1a_u32(instance_id.to_ascii_uppercase().as_bytes())
            );
            let kind = super::wireless_vendors_ext::classify_wireless_device_kind(
                0,
                0,
                &name,
                BatterySource::Bluetooth,
                &instance_id,
            );
            if seen.insert(id.clone()) {
                readings.push(reading_with_meta(
                    id,
                    name,
                    percent_01(level),
                    true,
                    false,
                    BatterySource::Bluetooth,
                    kind,
                    None,
                ));
            }
        }
        unsafe { SetupDiDestroyDeviceInfoList(set) };
    }

    // Secondary fallback: PnP registry ONLY for Bluetooth instances confirmed present by SetupDi.
    const BATTERY_LEVEL_PROPERTY: &str = "{104ea319-6ee2-4701-bd47-8ddbf425bbe5} 2";
    const BATTERY_LIFE_PROPERTY: &str = "System.Devices.BatteryLife";
    const NAME_KEYS: &[&str] = &["FriendlyName", "Name", "DisplayName"];
    const LEVEL_KEYS: &[&str] = &[
        BATTERY_LEVEL_PROPERTY,
        BATTERY_LIFE_PROPERTY,
        "BatteryLevel",
        "BatteryPercent",
    ];
    const ROOTS: &[(Handle, &str, &str)] = &[
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Enum\Bluetooth",
            "BLUETOOTH",
        ),
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Enum\BTHENUM",
            "BTHENUM",
        ),
    ];

    let Some(present_instances) = collect_present_bluetooth_instances(cancel) else {
        return readings;
    };
    if present_instances.is_empty() {
        return readings;
    }

    for &(root, path, prefix) in ROOTS {
        if cancel.load(Ordering::Relaxed) || readings.len() >= 32 {
            break;
        }
        let Some(key) = RegKey::open(root, path) else {
            continue;
        };
        for family in reg_subkeys(key.raw()).into_iter().take(64) {
            if cancel.load(Ordering::Relaxed) || readings.len() >= 32 {
                break;
            }
            let Some(family_key) = RegKey::open(key.raw(), &family) else {
                continue;
            };
            for instance in reg_subkeys(family_key.raw()).into_iter().take(64) {
                if cancel.load(Ordering::Relaxed) || readings.len() >= 32 {
                    break;
                }
                let full_instance = format!("{prefix}\\{family}\\{instance}").to_ascii_uppercase();
                if !present_instances.contains(&full_instance) {
                    continue;
                }
                let Some(node) = RegKey::open(family_key.raw(), &instance) else {
                    continue;
                };
                let level = LEVEL_KEYS
                    .iter()
                    .find_map(|name| reg_percent(node.raw(), name))
                    .or_else(|| {
                        RegKey::open(node.raw(), "Device Parameters").and_then(|params| {
                            LEVEL_KEYS
                                .iter()
                                .find_map(|name| reg_percent(params.raw(), name))
                        })
                    });
                let Some(level) = level else {
                    continue;
                };
                let raw_name = NAME_KEYS
                    .iter()
                    .find_map(|name| reg_string(node.raw(), name))
                    .unwrap_or_else(|| normalize_name(&instance));
                let name = super::wireless_vendors_ext::clean_bluetooth_device_name(&raw_name);
                if is_ignored_wired_device(&name) {
                    continue;
                }
                let id = format!("bt-pnp:{:08X}", fnv1a_u32(full_instance.as_bytes()));
                let kind = super::wireless_vendors_ext::classify_wireless_device_kind(
                    0,
                    0,
                    &name,
                    BatterySource::Bluetooth,
                    &full_instance,
                );
                if seen.insert(id.clone()) {
                    readings.push(reading_with_meta(
                        id,
                        name,
                        percent_01(level),
                        true,
                        false,
                        BatterySource::Bluetooth,
                        kind,
                        None,
                    ));
                }
            }
        }
    }
    readings
}

/// Minimal RFC 6455 client for G-Hub's loopback WebSocket (`ws://127.0.0.1:9010`).
fn ghub_websocket_json(cancel: &AtomicBool) -> Option<String> {
    const ENDPOINT: &str = "ws://127.0.0.1:9010";
    if !is_valid_ghub_uri(ENDPOINT) {
        return None;
    }
    let authority = parse_loopback_endpoint(ENDPOINT, 9010, true)?;
    let mut stream = TcpStream::connect_timeout(
        &authority.to_socket_addrs().ok()?.next()?,
        Duration::from_millis(300),
    )
    .ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));

    let request = format!(
        "GET / HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\
         Origin: file://\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut headers = Vec::new();
    let mut byte = [0u8; 1];
    while headers.len() < 8192 {
        if stream.read_exact(&mut byte).is_err() || cancel.load(Ordering::Relaxed) {
            return None;
        }
        headers.push(byte[0]);
        if headers.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    if !String::from_utf8_lossy(&headers).starts_with("HTTP/1.1 101") {
        return None;
    }

    let payload = br#"{"msgId":"ot-1","verb":"GET","path":"/devices/list"}"#;
    let mask = [
        (std::process::id() & 0xff) as u8,
        (std::process::id() >> 8) as u8,
        0x5a,
        0xa3,
    ];
    let mut frame = vec![0x81, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    stream.write_all(&frame).ok()?;

    let mut head = [0u8; 2];
    stream.read_exact(&mut head).ok()?;
    let mut len = (head[1] & 0x7f) as usize;
    if len == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext).ok()?;
        len = u16::from_be_bytes(ext) as usize;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext).ok()?;
        len = u64::from_be_bytes(ext) as usize;
    }
    if len == 0 || len > 256 * 1024 {
        return None;
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    Some(String::from_utf8_lossy(&body).into_owned())
}

fn map_loopback_readings(prefix: &str, items: Vec<BatteryReading>) -> Vec<WirelessReading> {
    items
        .into_iter()
        .map(|b| {
            reading(
                format!("{prefix}:{}", b.id),
                b.name,
                percent_01(b.level),
                b.connected,
                b.charging,
                b.source,
            )
        })
        .collect()
}

fn probe_ghub_loopback(cancel: &AtomicBool) -> Vec<WirelessReading> {
    ghub_websocket_json(cancel)
        .map(|json| map_loopback_readings("ghub", parse_ghub_json(&json)))
        .unwrap_or_default()
}

fn probe_steelseries_loopback(cancel: &AtomicBool) -> Vec<WirelessReading> {
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    let path = program_data
        .join("SteelSeries")
        .join("SteelSeries Engine 3")
        .join("coreProps.json");
    let Ok(json) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Some(url) = parse_coreprops_endpoint(&json) else {
        return Vec::new();
    };
    let Some(port) = url_port(&url) else {
        return Vec::new();
    };
    let Some(authority) = parse_loopback_endpoint(&url, port, false) else {
        return Vec::new();
    };
    for relative in ["/battery", "/devices", "/game_metadata"] {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(body) = crate::platform::http::loopback_get(&authority, relative) else {
            continue;
        };
        let readings = parse_steelseries_gg_json(&body);
        if !readings.is_empty() {
            return map_loopback_readings("ss-gg", readings);
        }
    }
    Vec::new()
}

fn url_port(url: &str) -> Option<u16> {
    let authority = url.split("://").nth(1)?.split('/').next()?;
    authority
        .rsplit_once(':')
        .map(|(_, port)| port.parse().ok())?
}

// ---------------------------------------------------------------------------
// Sağlayıcı 7-12: canlı çoklu-marka vendor HID protokolleri
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct ProbePolicy {
    listen_only: bool,
    multi_brand_enabled: bool,
    disabled_vendors: Vec<String>,
}

impl Default for ProbePolicy {
    fn default() -> Self {
        Self {
            listen_only: false,
            multi_brand_enabled: true,
            disabled_vendors: Vec::new(),
        }
    }
}

impl From<&AggregatorSettings> for ProbePolicy {
    fn from(settings: &AggregatorSettings) -> Self {
        Self {
            listen_only: settings.listen_only,
            multi_brand_enabled: settings.multi_brand_enabled,
            disabled_vendors: settings.disabled_vendors.clone(),
        }
    }
}

fn is_multi_brand_source(source: BatterySource) -> bool {
    matches!(
        source,
        BatterySource::AsusRogHid
            | BatterySource::CorsairHid
            | BatterySource::HyperXHid
            | BatterySource::CompxAtkVxe
            | BatterySource::Keyboard24gHid
            | BatterySource::GamepadHid
    )
}

fn source_vendor_tokens(source: BatterySource) -> &'static [&'static str] {
    match source {
        BatterySource::LogitechHidPlusPlus | BatterySource::LogitechGHub => {
            &["logitech", "hid++", "ghub"]
        }
        BatterySource::RazerHid => &["razer"],
        BatterySource::SteelSeriesHid | BatterySource::SteelSeriesGg => &["steelseries", "ss"],
        BatterySource::AsusRogHid => &["asus", "rog", "asusrog"],
        BatterySource::CorsairHid => &["corsair"],
        BatterySource::HyperXHid => &["hyperx", "hp", "kingston"],
        BatterySource::CompxAtkVxe => &["compx", "atk", "vxe"],
        BatterySource::Keyboard24gHid => &["keyboard24g", "2.4g"],
        BatterySource::GamepadHid => &["gamepad", "sony", "nintendo"],
        BatterySource::XInput => &["xinput", "xbox"],
        BatterySource::Bluetooth | BatterySource::BluetoothGatt => &["bluetooth", "ble"],
        BatterySource::StandardHid => &["hid", "standardhid"],
    }
}

impl ProbePolicy {
    fn has_disabled_token(&self, token: &str) -> bool {
        self.disabled_vendors
            .iter()
            .any(|v| v.trim().eq_ignore_ascii_case(token))
    }

    fn is_source_disabled(&self, source: BatterySource) -> bool {
        if self.listen_only && !source.is_passive() {
            return true;
        }
        if !self.multi_brand_enabled && is_multi_brand_source(source) {
            return true;
        }
        source_vendor_tokens(source)
            .iter()
            .any(|tok| self.has_disabled_token(tok))
    }

    fn allows_source(&self, source: BatterySource) -> bool {
        !self.is_source_disabled(source)
    }

    fn is_vid_disabled(&self, vid: u16, product: &str) -> bool {
        if self.disabled_vendors.is_empty() {
            return false;
        }
        if let Some(tok) = super::wireless_vendors_ext::vendor_token_for_vid(vid)
            && self.has_disabled_token(tok)
        {
            return true;
        }
        super::wireless_vendors_ext::is_glorious_device(vid, 0, product)
            && self.has_disabled_token("glorious")
    }
}

fn build_corsair_legacy_request(report_len: usize) -> Vec<u8> {
    let len = report_len.max(64);
    let mut packet = vec![0u8; len];
    let offset = usize::from(len == 65);
    packet[offset] = 0xC9;
    packet[offset + 1] = 0x64;
    packet
}

fn build_nordic64_request(report_len: usize) -> Vec<u8> {
    let len = if report_len == 65 { 65 } else { 64 };
    let mut packet = vec![0u8; len];
    packet[0] = 0x08;
    packet[1] = COMPX_QUERY_CMD;
    packet[16] = 0x49;
    packet
}

fn build_atk1k_feature_request(report_len: usize, report_id_zero: bool) -> Vec<u8> {
    let mut packet = vec![0u8; report_len.max(9)];
    if report_id_zero {
        packet[0] = 0x00;
        packet[1] = 0x82;
    } else {
        packet[0] = 0x82;
    }
    packet
}

fn build_keyboard_24g_query(expected_len: usize, buffer_len: usize) -> Vec<u8> {
    if expected_len == COMPX_REPORT_LEN {
        return build_compx_request().to_vec();
    }
    let len = buffer_len.max(64);
    let mut packet = vec![0u8; len];
    let offset = usize::from(len == 65);
    packet[offset] = 0x08;
    packet[offset + 1] = COMPX_QUERY_CMD;
    packet[offset + 16] = 0x49;
    packet
}

fn probe_asus_rog(
    devices: &[HidDevice],
    cancel: &AtomicBool,
    policy: &ProbePolicy,
) -> Vec<WirelessReading> {
    if !policy.allows_source(BatterySource::AsusRogHid) {
        return Vec::new();
    }
    let budget = Budget::new(900);
    let mut result = Vec::new();
    let mut seen_pids = HashSet::new();
    for device in devices {
        if budget.expired(cancel) {
            break;
        }
        if device.vid != ASUS_VID
            || !matches!(device.usage_page, 0xFF00 | 0xFF01)
            || !matches!(device.input_len, 64 | 65)
            || !matches!(device.output_len, 64 | 65)
            || is_ignored_wired_product(&device.product)
            || seen_pids.contains(&device.pid)
        {
            continue;
        }
        let Some(session) = HidSession::open(&device.path, false) else {
            continue;
        };
        let out_len = (device.output_len as usize).max(64);
        let in_len = (device.input_len as usize).max(64);
        let request = build_asus_request(out_len);
        let mut response = vec![0u8; in_len];
        let Some(bytes) = session.transact(
            &request,
            &mut response,
            220,
            |pkt| {
                let r = normalize_framing(pkt);
                r.len() >= 6 && r[0] == ASUS_CMD_CATEGORY && r[1] == ASUS_CMD_GET_BATTERY
            },
            cancel,
        ) else {
            continue;
        };
        let name = if device.product.trim().is_empty() {
            "ASUS ROG Wireless".to_owned()
        } else {
            device.product.trim().to_owned()
        };
        let id = device.id("rog");
        if let Some(parsed) = parse_asus_rog(&response[..bytes], &id, &name) {
            let kind = super::wireless_vendors_ext::classify_wireless_device_kind(
                device.vid,
                device.pid,
                &parsed.name,
                BatterySource::AsusRogHid,
                &parsed.id,
            );
            seen_pids.insert(device.pid);
            result.push(reading_with_meta(
                parsed.id,
                parsed.name,
                percent_01(parsed.level),
                parsed.connected,
                parsed.charging,
                BatterySource::AsusRogHid,
                kind,
                None,
            ));
        }
    }
    let fallback = probe_standard_hid(devices, cancel, BatterySource::AsusRogHid, |d| {
        d.vid == ASUS_VID && !seen_pids.contains(&d.pid)
    });
    result.extend(fallback);
    result
}

fn probe_corsair(
    devices: &[HidDevice],
    cancel: &AtomicBool,
    policy: &ProbePolicy,
) -> Vec<WirelessReading> {
    if !policy.allows_source(BatterySource::CorsairHid) {
        return Vec::new();
    }
    let budget = Budget::new(950);
    let mut result = Vec::new();
    let mut seen_pids = HashSet::new();
    for device in devices {
        if budget.expired(cancel) {
            break;
        }
        if device.vid != CORSAIR_VID
            || !matches!(
                device.usage_page,
                BRAGI_USAGE_PAGE | CORSAIR_LEGACY_USAGE_PAGE
            )
            || !matches!(device.input_len, 64 | 65)
            || !matches!(device.output_len, 64 | 65)
            || is_ignored_wired_product(&device.product)
            || seen_pids.contains(&device.pid)
        {
            continue;
        }
        let Some(session) = HidSession::open(&device.path, false) else {
            continue;
        };
        let out_len = (device.output_len as usize).max(64);
        let in_len = (device.input_len as usize).max(64);
        let name = if device.product.trim().is_empty() {
            "Corsair Wireless".to_owned()
        } else {
            device.product.trim().to_owned()
        };
        let id = device.id("corsair");
        let kind = super::wireless_vendors_ext::classify_wireless_device_kind(
            device.vid,
            device.pid,
            &name,
            BatterySource::CorsairHid,
            &id,
        );

        if device.usage_page == BRAGI_USAGE_PAGE {
            // Query Slipstream sub-device (0x09) AND direct host endpoint (0x08).
            for endpoint in [0x09u8, 0x08u8] {
                if budget.expired(cancel) {
                    break;
                }
                let level_req = build_bragi_request(endpoint, BRAGI_PROPERTY_LEVEL, out_len);
                let mut level_resp = vec![0u8; in_len];
                let Some(read_len) =
                    session.transact(&level_req, &mut level_resp, 220, valid_bragi, cancel)
                else {
                    continue;
                };
                let Some(level) = parse_corsair_bragi_level(&level_resp[..read_len]) else {
                    continue;
                };
                let mut charging = false;
                let status_req = build_bragi_request(endpoint, BRAGI_PROPERTY_STATUS, out_len);
                let mut status_resp = vec![0u8; in_len];
                if let Some(status_len) =
                    session.transact(&status_req, &mut status_resp, 180, valid_bragi, cancel)
                    && let Some(ch) = parse_corsair_bragi_charging(&status_resp[..status_len])
                {
                    charging = ch;
                }
                seen_pids.insert(device.pid);
                result.push(reading_with_meta(
                    id.clone(),
                    name.clone(),
                    percent_01(level as i32),
                    true,
                    charging,
                    BatterySource::CorsairHid,
                    kind,
                    None,
                ));
                break;
            }
        } else if device.usage_page == CORSAIR_LEGACY_USAGE_PAGE {
            let req = build_corsair_legacy_request(out_len);
            let mut resp = vec![0u8; in_len];
            if let Some(read_len) = session.transact(
                &req,
                &mut resp,
                220,
                |pkt| {
                    let r = corsair_framing(pkt);
                    r.len() >= 5 && r[0] == 0xC9 && r[1] == 0x64
                },
                cancel,
            ) && let Some(parsed) = parse_corsair_legacy(&resp[..read_len], &id, &name)
            {
                seen_pids.insert(device.pid);
                result.push(reading_with_meta(
                    parsed.id,
                    parsed.name,
                    percent_01(parsed.level),
                    parsed.connected,
                    parsed.charging,
                    BatterySource::CorsairHid,
                    WirelessDeviceKind::Headphones,
                    None,
                ));
            }
        }
    }
    let fallback = probe_standard_hid(devices, cancel, BatterySource::CorsairHid, |d| {
        d.vid == CORSAIR_VID && !seen_pids.contains(&d.pid)
    });
    result.extend(fallback);
    result
}

fn probe_hyperx(
    devices: &[HidDevice],
    cancel: &AtomicBool,
    policy: &ProbePolicy,
) -> Vec<WirelessReading> {
    use super::wireless_vendors_ext::{
        HP_HYPERX_VID, KINGSTON_HYPERX_VID, build_hyperx_protocol_request, hyperx_profile,
        is_valid_hyperx_protocol_reply, parse_hyperx_protocol,
    };

    if !policy.allows_source(BatterySource::HyperXHid) {
        return Vec::new();
    }
    let budget = Budget::new(950);
    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for device in devices {
        if budget.expired(cancel) {
            break;
        }
        if !matches!(device.vid, HP_HYPERX_VID | KINGSTON_HYPERX_VID)
            || !matches!(device.usage_page, 0xFF00 | 0xFF13 | 0xFF90 | 0xFFC0)
            || !(20..=65).contains(&device.input_len)
            || !(20..=65).contains(&device.output_len)
            || is_ignored_wired_product(&device.product)
            || seen.contains(&(device.vid, device.pid))
        {
            continue;
        }
        let id = device.id("hyperx");

        if let Some(profile) = hyperx_profile(device.vid, device.pid) {
            let Some(session) = HidSession::open(&device.path, false) else {
                continue;
            };
            let out_len = (device.output_len as usize).max(20);
            let in_len = (device.input_len as usize).max(20);
            let request = build_hyperx_protocol_request(profile.protocol, out_len);
            let mut response = vec![0u8; in_len];
            let Some(bytes) = session.transact(
                &request,
                &mut response,
                240,
                |pkt| is_valid_hyperx_protocol_reply(profile.protocol, pkt),
                cancel,
            ) else {
                continue;
            };
            let name = if device.product.trim().is_empty() {
                profile.default_name.to_owned()
            } else {
                device.product.trim().to_owned()
            };
            if let Some(parsed) = parse_hyperx_protocol(
                profile.protocol,
                &response[..bytes],
                &id,
                &name,
                profile.device_kind,
            ) {
                seen.insert((device.vid, device.pid));
                result.push(reading_with_meta(
                    parsed.id,
                    parsed.name,
                    percent_01(parsed.level),
                    parsed.connected,
                    parsed.charging,
                    BatterySource::HyperXHid,
                    parsed.device_kind,
                    None,
                ));
            }
        } else if let Some(family) = hyperx_family(device.usage_page, device.vid) {
            // Only probe unknown HP/Kingston PIDs when their product string explicitly identifies HyperX.
            let lower = device.product.to_ascii_lowercase();
            if !(lower.contains("hyperx") || lower.contains("cloud") || lower.contains("pulsefire"))
            {
                continue;
            }
            let Some(session) = HidSession::open(&device.path, false) else {
                continue;
            };
            let out_len = (device.output_len as usize).max(64);
            let in_len = (device.input_len as usize).max(64);
            let request = build_hyperx_request(family, out_len);
            let mut response = vec![0u8; in_len];
            if let Some(bytes) = session.transact(
                &request,
                &mut response,
                220,
                |pkt| valid_hyperx(family, pkt),
                cancel,
            ) && let Some(parsed) =
                parse_hyperx(family, &response[..bytes], &id, &device.product)
            {
                let kind = super::wireless_vendors_ext::classify_wireless_device_kind(
                    device.vid,
                    device.pid,
                    &parsed.name,
                    BatterySource::HyperXHid,
                    &parsed.id,
                );
                seen.insert((device.vid, device.pid));
                result.push(reading_with_meta(
                    parsed.id,
                    parsed.name,
                    percent_01(parsed.level),
                    parsed.connected,
                    parsed.charging,
                    BatterySource::HyperXHid,
                    kind,
                    None,
                ));
            }
        }
    }

    let fallback = probe_standard_hid(devices, cancel, BatterySource::HyperXHid, |d| {
        matches!(d.vid, HP_HYPERX_VID | KINGSTON_HYPERX_VID)
            && !seen.contains(&(d.vid, d.pid))
            && (hyperx_profile(d.vid, d.pid).is_some()
                || d.product.to_ascii_lowercase().contains("hyperx"))
    });
    result.extend(fallback);
    result
}

fn is_verified_keyboard_24g_profile(vid: u16, pid: u16) -> Option<(u16, usize)> {
    match (vid, pid) {
        (0x258A, 0x010C | 0x013C | 0x0152 | 0x00B5) => Some((0xFF00, 64)),
        (0x3554, 0xFA09) => Some((0xFF02, 17)),
        (0x3151, 0x4011 | 0x4015) => Some((0xFF02, 64)),
        _ => None,
    }
}

fn probe_compx_and_mice(
    devices: &[HidDevice],
    cancel: &AtomicBool,
    policy: &ProbePolicy,
) -> Vec<WirelessReading> {
    use super::wireless_vendors_ext::{
        ENDGAME_GEAR_VID, FINALMOUSE_VID, KEYCHRON_VENDOR_USAGE_PAGE, KEYCHRON_VID,
        build_endgame_gear_request, build_finalmouse_request, build_glorious_request,
        build_zowie_request, compx_default_model_name, endgame_gear_model_name,
        finalmouse_model_name, is_glorious_device, is_glorious_keyboard, is_keychron_keyboard,
        is_valid_endgame_gear_reply, is_valid_finalmouse_reply, is_valid_zowie_reply,
        is_zowie_device, parse_endgame_gear_reply, parse_finalmouse_reply, parse_glorious_reply,
        parse_zowie_reply, zowie_model_name,
    };

    if !policy.allows_source(BatterySource::CompxAtkVxe) {
        return Vec::new();
    }
    let budget = Budget::new(1000);
    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for device in devices {
        if budget.expired(cancel) {
            break;
        }
        if is_ignored_wired(device.vid, device.pid, &device.product)
            || is_ignored_wired_product(&device.product)
            || policy.is_vid_disabled(device.vid, &device.product)
            || seen.contains(&(device.vid, device.pid))
        {
            continue;
        }
        // Leave dedicated 2.4G / Keychron / GMMK keyboards to Keyboard24gHid.
        if is_verified_keyboard_24g_profile(device.vid, device.pid).is_some()
            || is_keychron_keyboard(device.vid, device.pid, &device.product, device.usage_page)
            || is_glorious_keyboard(device.vid, device.pid, &device.product)
        {
            continue;
        }

        let id = device.id("compx");

        // 1. Glorious Wireless Mice (65-byte Feature Report)
        if is_glorious_device(device.vid, device.pid, &device.product)
            && matches!(device.usage_page, 0xFF00..=0xFF02)
            && device.feature_len >= 64
        {
            let Some(session) = HidSession::open(&device.path, true) else {
                continue;
            };
            let feat_len = (device.feature_len as usize).max(65);
            let req = build_glorious_request(feat_len);
            if session.set_feature(&req) {
                unsafe { Sleep(20) };
                let mut resp = vec![0u8; feat_len];
                if session.get_feature(&mut resp)
                    && let Some(parsed) =
                        parse_glorious_reply(&resp, device.vid, device.pid, &id, &device.product)
                {
                    seen.insert((device.vid, device.pid));
                    result.push(reading_with_meta(
                        parsed.id,
                        parsed.name,
                        percent_01(parsed.level),
                        parsed.connected,
                        parsed.charging,
                        parsed.source,
                        parsed.device_kind,
                        None,
                    ));
                }
            }
            continue;
        }

        // 2. Endgame Gear (0x3367: OP1we, XM2we, XM2w 4k, OP1w 4k)
        if device.vid == ENDGAME_GEAR_VID
            && matches!(device.usage_page, 0xFF00 | 0xFF02 | 0xFFA0)
            && (matches!(device.input_len, 17 | 64 | 65)
                && matches!(device.output_len, 17 | 64 | 65))
        {
            let Some(session) = HidSession::open(&device.path, false) else {
                continue;
            };
            let out_len = device.output_len as usize;
            let in_len = (device.input_len as usize).max(17);
            let name = if device.product.trim().is_empty() {
                endgame_gear_model_name(device.pid).to_owned()
            } else {
                device.product.trim().to_owned()
            };
            let req = build_endgame_gear_request(out_len);
            let mut resp = vec![0u8; in_len];
            let parsed = session
                .transact(&req, &mut resp, 220, is_valid_endgame_gear_reply, cancel)
                .and_then(|n| parse_endgame_gear_reply(&resp[..n], device.pid, &id, &name))
                .or_else(|| {
                    if out_len >= 64 {
                        let nordic_req = build_nordic64_request(out_len);
                        let mut nordic_resp = vec![0u8; in_len];
                        session
                            .transact(
                                &nordic_req,
                                &mut nordic_resp,
                                220,
                                is_valid_endgame_gear_reply,
                                cancel,
                            )
                            .and_then(|n| {
                                parse_endgame_gear_reply(&nordic_resp[..n], device.pid, &id, &name)
                            })
                    } else {
                        None
                    }
                });
            if let Some(parsed) = parsed {
                seen.insert((device.vid, device.pid));
                result.push(reading_with_meta(
                    parsed.id,
                    parsed.name,
                    percent_01(parsed.level),
                    parsed.connected,
                    parsed.charging,
                    BatterySource::CompxAtkVxe,
                    WirelessDeviceKind::Mouse,
                    None,
                ));
            }
            continue;
        }

        // 3. Finalmouse (0x361D: UltralightX / Starlight-12)
        if device.vid == FINALMOUSE_VID
            && matches!(device.usage_page, 0xFF00..=0xFF02)
            && matches!(device.input_len, 64 | 65)
            && matches!(device.output_len, 64 | 65)
        {
            let Some(session) = HidSession::open(&device.path, false) else {
                continue;
            };
            let out_len = (device.output_len as usize).max(64);
            let in_len = (device.input_len as usize).max(64);
            let name = if device.product.trim().is_empty() {
                finalmouse_model_name(device.pid).to_owned()
            } else {
                device.product.trim().to_owned()
            };
            let req = build_finalmouse_request(out_len);
            let mut resp = vec![0u8; in_len];
            if let Some(bytes) =
                session.transact(&req, &mut resp, 220, is_valid_finalmouse_reply, cancel)
                && let Some(parsed) = parse_finalmouse_reply(&resp[..bytes], device.pid, &id, &name)
            {
                seen.insert((device.vid, device.pid));
                result.push(reading_with_meta(
                    parsed.id,
                    parsed.name,
                    percent_01(parsed.level),
                    parsed.connected,
                    parsed.charging,
                    BatterySource::CompxAtkVxe,
                    WirelessDeviceKind::Mouse,
                    None,
                ));
            }
            continue;
        }

        // 4. Zowie / BenQ (0x3057 / 0x1EA7 Enhanced Receiver)
        if is_zowie_device(device.vid, device.pid, &device.product)
            && matches!(device.usage_page, 0xFF00 | 0xFF01 | 0xFF02 | 0xFFA0)
            && matches!(device.input_len, 64 | 65)
            && matches!(device.output_len, 64 | 65)
        {
            let Some(session) = HidSession::open(&device.path, false) else {
                continue;
            };
            let out_len = (device.output_len as usize).max(64);
            let in_len = (device.input_len as usize).max(64);
            let name = if device.product.trim().is_empty() {
                zowie_model_name(device.vid, device.pid).to_owned()
            } else {
                device.product.trim().to_owned()
            };
            let req = build_zowie_request(out_len);
            let mut resp = vec![0u8; in_len];
            if let Some(bytes) =
                session.transact(&req, &mut resp, 220, is_valid_zowie_reply, cancel)
                && let Some(parsed) =
                    parse_zowie_reply(&resp[..bytes], device.vid, device.pid, &id, &name)
            {
                seen.insert((device.vid, device.pid));
                result.push(reading_with_meta(
                    parsed.id,
                    parsed.name,
                    percent_01(parsed.level),
                    parsed.connected,
                    parsed.charging,
                    BatterySource::CompxAtkVxe,
                    WirelessDeviceKind::Mouse,
                    None,
                ));
            }
            continue;
        }

        // 5. All COMPX_VENDORS (ATK / VXE / VGN / MCHOSE / Scyrox / WLMouse / Lamzu / Ninjutso / Ajazz / Attack Shark / Darmoshark / Zaopin / Incott / Keychron / Dareu / Rapoo)
        if !is_compx_vendor(device.vid) {
            continue;
        }
        let is_compx17 = device.usage_page == 0xFF02
            && device.input_len as usize == COMPX_REPORT_LEN
            && device.output_len as usize == COMPX_REPORT_LEN;
        let is_atk1k = device.vid == 0x373B
            && device.pid == 0x10F0
            && device.usage_page == 0xFF10
            && device.feature_len >= 9
            && device.input_len >= 10;
        let is_nordic64 = (matches!(device.usage_page, 0xFF02 | 0xFF00)
            || (device.vid == KEYCHRON_VID && device.usage_page == KEYCHRON_VENDOR_USAGE_PAGE))
            && matches!(device.input_len, 64 | 65)
            && matches!(device.output_len, 64 | 65);
        if !is_compx17 && !is_atk1k && !is_nordic64 {
            continue;
        }

        let Some(session) = HidSession::open(&device.path, false) else {
            continue;
        };
        let name = if device.product.trim().is_empty() {
            compx_default_model_name(device.vid, device.pid).to_owned()
        } else {
            device.product.trim().to_owned()
        };

        let parsed = if is_compx17 {
            let req = build_compx_request();
            let mut resp = [0u8; COMPX_REPORT_LEN];
            session
                .transact(
                    &req,
                    &mut resp,
                    250,
                    |pkt| pkt.len() >= 10 && pkt[0] == 0x08 && pkt[1] == COMPX_QUERY_CMD,
                    cancel,
                )
                .and_then(|bytes| parse_compx17(&resp[..bytes], &id, &name))
        } else if is_atk1k {
            let feat_len = (device.feature_len as usize).max(9);
            let sent = session.set_feature(&build_atk1k_feature_request(feat_len, true))
                || session.set_feature(&build_atk1k_feature_request(feat_len, false));
            if sent {
                let in_len = (device.input_len as usize).max(65);
                let mut resp = vec![0u8; in_len];
                let mut found = None;
                for _ in 0..4 {
                    if budget.expired(cancel) {
                        break;
                    }
                    let Some(bytes) = session.read_report(&mut resp, 250) else {
                        break;
                    };
                    if let Some(reading) = parse_atk1k(&resp[..bytes], &id, &name) {
                        found = Some(reading);
                        break;
                    }
                }
                found
            } else {
                None
            }
        } else {
            let out_len = (device.output_len as usize).max(64);
            let in_len = (device.input_len as usize).max(64);
            let req = build_nordic64_request(out_len);
            let mut resp = vec![0u8; in_len];
            session
                .transact(
                    &req,
                    &mut resp,
                    250,
                    |pkt| {
                        let r = normalize_nordic(pkt);
                        (r.len() >= 10 && r[0] == 0x08 && r[1] == COMPX_QUERY_CMD)
                            || (r.len() >= 9 && r[0] == COMPX_QUERY_CMD)
                    },
                    cancel,
                )
                .and_then(|bytes| parse_compx_nordic(&resp[..bytes], &id, &name))
        };

        if let Some(parsed) = parsed {
            let kind = super::wireless_vendors_ext::classify_wireless_device_kind(
                device.vid,
                device.pid,
                &parsed.name,
                BatterySource::CompxAtkVxe,
                &parsed.id,
            );
            seen.insert((device.vid, device.pid));
            result.push(reading_with_meta(
                parsed.id,
                parsed.name,
                percent_01(parsed.level),
                parsed.connected,
                parsed.charging,
                BatterySource::CompxAtkVxe,
                kind,
                None,
            ));
        }
    }
    result
}

fn probe_keyboard_24g(
    devices: &[HidDevice],
    cancel: &AtomicBool,
    policy: &ProbePolicy,
) -> Vec<WirelessReading> {
    use super::wireless_vendors_ext::{
        build_glorious_request, build_keychron_raw_hid_request, is_glorious_keyboard,
        is_keychron_keyboard, is_valid_keychron_raw_hid_reply, keyboard_24g_default_name,
        parse_glorious_reply, parse_keychron_raw_hid_reply,
    };

    if !policy.allows_source(BatterySource::Keyboard24gHid) {
        return Vec::new();
    }
    let budget = Budget::new(950);
    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for device in devices {
        if budget.expired(cancel) {
            break;
        }
        if is_ignored_wired(device.vid, device.pid, &device.product)
            || is_ignored_wired_product(&device.product)
            || policy.is_vid_disabled(device.vid, &device.product)
            || seen.contains(&(device.vid, device.pid))
        {
            continue;
        }

        let id = device.id("kbd24g");

        // 1. Keychron wireless keyboards (0x3434, Raw HID 0xFF60 / 0xFFC1)
        if is_keychron_keyboard(device.vid, device.pid, &device.product, device.usage_page)
            && (32..=65).contains(&device.input_len)
            && (32..=65).contains(&device.output_len)
        {
            let Some(session) = HidSession::open(&device.path, false) else {
                continue;
            };
            let out_len = (device.output_len as usize).clamp(32, 65);
            let in_len = (device.input_len as usize).clamp(32, 65);
            let name = if device.product.trim().is_empty() {
                keyboard_24g_default_name(device.vid, device.pid).to_owned()
            } else {
                device.product.trim().to_owned()
            };
            let req = build_keychron_raw_hid_request(out_len);
            let mut resp = vec![0u8; in_len];
            if let Some(bytes) = session.transact(
                &req,
                &mut resp,
                220,
                is_valid_keychron_raw_hid_reply,
                cancel,
            ) && let Some(parsed) =
                parse_keychron_raw_hid_reply(&resp[..bytes], device.pid, &id, &name)
            {
                seen.insert((device.vid, device.pid));
                result.push(reading_with_meta(
                    parsed.id,
                    parsed.name,
                    percent_01(parsed.level),
                    parsed.connected,
                    parsed.charging,
                    BatterySource::Keyboard24gHid,
                    WirelessDeviceKind::Keyboard,
                    None,
                ));
            }
            continue;
        }

        // 2. Glorious GMMK Wireless keyboards (0x320F)
        if is_glorious_keyboard(device.vid, device.pid, &device.product)
            && matches!(device.usage_page, 0xFF00..=0xFF02)
            && device.feature_len >= 64
        {
            let Some(session) = HidSession::open(&device.path, true) else {
                continue;
            };
            let feat_len = (device.feature_len as usize).max(65);
            let req = build_glorious_request(feat_len);
            if session.set_feature(&req) {
                unsafe { Sleep(20) };
                let mut resp = vec![0u8; feat_len];
                if session.get_feature(&mut resp)
                    && let Some(parsed) =
                        parse_glorious_reply(&resp, device.vid, device.pid, &id, &device.product)
                {
                    seen.insert((device.vid, device.pid));
                    result.push(reading_with_meta(
                        parsed.id,
                        parsed.name,
                        percent_01(parsed.level),
                        parsed.connected,
                        parsed.charging,
                        BatterySource::Keyboard24gHid,
                        WirelessDeviceKind::Keyboard,
                        None,
                    ));
                }
            }
            continue;
        }

        // 3. Verified 2.4G keyboards: AULA F75/F99, Weikav NUT75, Royal Kludge, Compx FA09, MonsGeek, Akko
        let Some((required_page, expected_len)) =
            is_verified_keyboard_24g_profile(device.vid, device.pid)
        else {
            continue;
        };
        if device.usage_page != required_page {
            continue;
        }
        let len_ok = if expected_len == COMPX_REPORT_LEN {
            device.input_len as usize == COMPX_REPORT_LEN
                && device.output_len as usize == COMPX_REPORT_LEN
        } else {
            matches!(device.input_len, 64 | 65) && matches!(device.output_len, 64 | 65)
        };
        if !len_ok {
            continue;
        }
        let Some(session) = HidSession::open(&device.path, false) else {
            continue;
        };
        let out_len = (device.output_len as usize).max(expected_len);
        let in_len = (device.input_len as usize).max(expected_len);
        let req = build_keyboard_24g_query(expected_len, out_len);
        let mut resp = vec![0u8; in_len];
        let Some(bytes) = session.transact(
            &req,
            &mut resp,
            240,
            |pkt| {
                if expected_len == COMPX_REPORT_LEN {
                    pkt.len() >= 10 && pkt[0] == 0x08 && pkt[1] == COMPX_QUERY_CMD
                } else {
                    let p = normalize_keyboard_24g_framing(pkt);
                    (p.len() >= 10 && p[0] == 0x08 && p[1] == COMPX_QUERY_CMD)
                        || (p.len() >= 9 && p[0] == COMPX_QUERY_CMD)
                        || (p.len() >= 3 && p[0] == 0x83 && p[1] <= 1)
                }
            },
            cancel,
        ) else {
            continue;
        };
        let name = if device.product.trim().is_empty() {
            keyboard_24g_default_name(device.vid, device.pid).to_owned()
        } else {
            device.product.trim().to_owned()
        };
        if let Some(parsed) = parse_keyboard_24g(&resp[..bytes], expected_len, &id, &name) {
            seen.insert((device.vid, device.pid));
            result.push(reading_with_meta(
                parsed.id,
                parsed.name,
                percent_01(parsed.level),
                parsed.connected,
                parsed.charging,
                BatterySource::Keyboard24gHid,
                WirelessDeviceKind::Keyboard,
                None,
            ));
        }
    }

    let fallback = probe_standard_hid(devices, cancel, BatterySource::Keyboard24gHid, |d| {
        if seen.contains(&(d.vid, d.pid)) {
            return false;
        }
        let lower = d.product.to_ascii_lowercase();
        (lower.contains("2.4g") || lower.contains("2,4g"))
            && (lower.contains("keyboard") || lower.contains("klavye"))
    });
    result.extend(fallback);
    result
}

fn default_gamepad_name(vid: u16, pid: u16) -> &'static str {
    match (vid, pid) {
        (0x054C, 0x0CE6) => "DualSense Wireless Controller",
        (0x054C, 0x0DF2) => "DualSense Edge Wireless Controller",
        (0x054C, 0x05C4 | 0x09CC) => "DUALSHOCK 4 Wireless Controller",
        (0x057E, 0x2009) => "Switch Pro Controller",
        _ => "Wireless Controller",
    }
}

fn is_supported_gamepad_pid(vid: u16, pid: u16) -> bool {
    matches!(
        (vid, pid),
        (0x054C, 0x0CE6 | 0x0DF2 | 0x05C4 | 0x09CC) | (0x057E, 0x2009)
    )
}

fn probe_gamepad_hid(
    devices: &[HidDevice],
    cancel: &AtomicBool,
    policy: &ProbePolicy,
) -> Vec<WirelessReading> {
    if !policy.allows_source(BatterySource::GamepadHid) {
        return Vec::new();
    }
    let budget = Budget::new(800);
    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for device in devices {
        if budget.expired(cancel) {
            break;
        }
        if !is_supported_gamepad_pid(device.vid, device.pid)
            || device.usage_page != 0x0001
            || !matches!(device.usage, 0x0004 | 0x0005)
            || device.input_len < 16
            || policy.is_vid_disabled(device.vid, &device.product)
            || seen.contains(&(device.vid, device.pid))
        {
            continue;
        }
        // Strictly read-only HID access (safe even in listen_only mode).
        let Some(session) = HidSession::open_read_only(&device.path)
            .or_else(|| HidSession::open(&device.path, true))
        else {
            continue;
        };
        let in_len = (device.input_len as usize).max(78);
        let id = device.id("gamepad");
        let name = if device.product.trim().is_empty() {
            default_gamepad_name(device.vid, device.pid).to_owned()
        } else {
            device.product.trim().to_owned()
        };

        let mut buffer = vec![0u8; in_len];
        let mut parsed = session
            .read_report(&mut buffer, 120)
            .and_then(|bytes| parse_gamepad(device.vid, device.pid, &buffer[..bytes], &id, &name));

        // DualSense / DS4 over Bluetooth may start in short 0x01 mode until a read-only
        // calibration feature (0x05) or explicit 0x31 / 0x11 input report is queried.
        if parsed.is_none() && device.vid == 0x054C {
            let feat_len = (device.feature_len as usize).max(41);
            if feat_len <= 128 {
                let mut cal = vec![0u8; feat_len];
                cal[0] = 0x05;
                let _ = session.get_feature(&mut cal);
            }
            for report_id in [0x31u8, 0x11u8, 0x01u8] {
                if budget.expired(cancel) {
                    break;
                }
                buffer.fill(0);
                buffer[0] = report_id;
                if session.get_input_report(&mut buffer)
                    && let Some(r) = parse_gamepad(device.vid, device.pid, &buffer, &id, &name)
                {
                    parsed = Some(r);
                    break;
                }
            }
            if parsed.is_none()
                && let Some(bytes) = session.read_report(&mut buffer, 120)
            {
                parsed = parse_gamepad(device.vid, device.pid, &buffer[..bytes], &id, &name);
            }
        }

        if let Some(parsed) = parsed {
            seen.insert((device.vid, device.pid));
            result.push(reading_with_meta(
                parsed.id,
                parsed.name,
                percent_01(parsed.level),
                parsed.connected,
                parsed.charging,
                BatterySource::GamepadHid,
                WirelessDeviceKind::Gamepad,
                None,
            ));
        }
    }

    let fallback = probe_standard_hid(devices, cancel, BatterySource::GamepadHid, |d| {
        d.usage_page == 0x0001
            && matches!(d.usage, 0x0004 | 0x0005)
            && !seen.contains(&(d.vid, d.pid))
    });
    result.extend(fallback);
    result
}

fn probe_vendor_hid(
    devices: &[HidDevice],
    cancel: &AtomicBool,
    source: BatterySource,
    policy: &ProbePolicy,
) -> Vec<WirelessReading> {
    if !policy.allows_source(source) {
        return Vec::new();
    }
    match source {
        BatterySource::AsusRogHid => probe_asus_rog(devices, cancel, policy),
        BatterySource::CorsairHid => probe_corsair(devices, cancel, policy),
        BatterySource::HyperXHid => probe_hyperx(devices, cancel, policy),
        BatterySource::CompxAtkVxe => probe_compx_and_mice(devices, cancel, policy),
        BatterySource::Keyboard24gHid => probe_keyboard_24g(devices, cancel, policy),
        BatterySource::GamepadHid => probe_gamepad_hid(devices, cancel, policy),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Toplayıcı: anlık görüntü, izole thread'ler, dedup, 30 dk stale önbellek
// ---------------------------------------------------------------------------

struct CachedReading {
    reading: WirelessReading,
    sampled_at: Instant,
}

static CACHE: OnceLock<Mutex<HashMap<String, CachedReading>>> = OnceLock::new();
static POLL_GATE: OnceLock<Mutex<()>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, CachedReading>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_cache() -> std::sync::MutexGuard<'static, HashMap<String, CachedReading>> {
    cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn evict_and_collect_stale(
    guard: &mut HashMap<String, CachedReading>,
    now: Instant,
    active: Option<&HashSet<String>>,
    out: &mut Vec<WirelessReading>,
) {
    let mut expired = Vec::new();
    for (key, cached) in guard.iter() {
        if now.saturating_duration_since(cached.sampled_at) > STALE_WINDOW {
            expired.push(key.clone());
            continue;
        }
        if active.is_some_and(|set| set.contains(key)) {
            continue;
        }
        let mut item = cached.reading.clone();
        item.stale = true;
        out.push(item);
    }
    for key in expired {
        guard.remove(&key);
    }
}

fn cached_snapshot() -> Vec<WirelessReading> {
    let mut entries = Vec::new();
    {
        let mut guard = lock_cache();
        evict_and_collect_stale(&mut guard, Instant::now(), None, &mut entries);
    }
    sort_readings(&mut entries);
    entries
}

fn sort_readings(readings: &mut [WirelessReading]) {
    readings.sort_by(|a, b| {
        a.stale
            .cmp(&b.stale)
            .then_with(|| a.is_charging.cmp(&b.is_charging))
            .then_with(|| {
                a.level_01
                    .partial_cmp(&b.level_01)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| source_priority(a.source).cmp(&source_priority(b.source)))
            .then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
    });
}

fn finalize(fresh: Vec<WirelessReading>) -> Vec<WirelessReading> {
    let now = Instant::now();
    let mut deduplicated: HashMap<String, WirelessReading> = HashMap::new();
    for item in fresh {
        if is_ignored_wired_reading(&item) {
            continue;
        }
        let key = canonical_key(&item);
        match deduplicated.get_mut(&key) {
            Some(existing) if prefer(&item, existing) => {
                let mut preferred = item;
                if preferred.secondary_level.is_none() {
                    preferred.secondary_level = existing.secondary_level;
                }
                if preferred.device_kind == WirelessDeviceKind::Other
                    && existing.device_kind != WirelessDeviceKind::Other
                {
                    preferred.device_kind = existing.device_kind;
                }
                *existing = preferred;
            }
            Some(existing) => {
                if existing.secondary_level.is_none() {
                    existing.secondary_level = item.secondary_level;
                }
                if existing.device_kind == WirelessDeviceKind::Other
                    && item.device_kind != WirelessDeviceKind::Other
                {
                    existing.device_kind = item.device_kind;
                }
            }
            None => {
                deduplicated.insert(key, item);
            }
        }
    }
    let active: HashSet<String> = deduplicated.keys().cloned().collect();
    let mut output = Vec::new();
    {
        let mut guard = lock_cache();
        for (key, mut item) in deduplicated {
            item.stale = false;
            guard.insert(
                key,
                CachedReading {
                    reading: item.clone(),
                    sampled_at: now,
                },
            );
            output.push(item);
        }
        evict_and_collect_stale(&mut guard, now, Some(&active), &mut output);
    }
    sort_readings(&mut output);
    output
}

fn run_probe(
    source: BatterySource,
    devices: &[HidDevice],
    cancel: &AtomicBool,
    policy: &ProbePolicy,
) -> Vec<WirelessReading> {
    if !policy.allows_source(source) {
        return Vec::new();
    }
    match source {
        BatterySource::XInput => probe_xinput(cancel),
        BatterySource::StandardHid => {
            probe_standard_hid(devices, cancel, BatterySource::StandardHid, |device| {
                !is_custom_protocol_device(device)
            })
        }
        BatterySource::LogitechHidPlusPlus => probe_logitech(devices, cancel),
        BatterySource::RazerHid => probe_razer(devices, cancel),
        BatterySource::SteelSeriesHid => probe_steelseries(devices, cancel),
        BatterySource::BluetoothGatt => probe_ble_gatt(devices, cancel),
        BatterySource::AsusRogHid
        | BatterySource::CorsairHid
        | BatterySource::HyperXHid
        | BatterySource::CompxAtkVxe
        | BatterySource::GamepadHid
        | BatterySource::Keyboard24gHid => probe_vendor_hid(devices, cancel, source, policy),
        // WS-4: real passive transports (SetupDi PnP + loopback JSON) feed live readings.
        BatterySource::Bluetooth => bluetooth_pnp_readings(cancel),
        BatterySource::LogitechGHub => probe_ghub_loopback(cancel),
        BatterySource::SteelSeriesGg => probe_steelseries_loopback(cancel),
    }
}

static SOURCE_IN_FLIGHT: [AtomicBool; 16] = [const { AtomicBool::new(false) }; 16];

fn source_slot(source: BatterySource) -> usize {
    let slot = source as usize;
    debug_assert!(slot < SOURCE_IN_FLIGHT.len());
    slot
}

struct SourceInFlightGuard {
    slot: usize,
}

impl Drop for SourceInFlightGuard {
    fn drop(&mut self) {
        SOURCE_IN_FLIGHT[self.slot].store(false, Ordering::Release);
    }
}

fn try_claim_source(source: BatterySource) -> Option<SourceInFlightGuard> {
    let slot = source_slot(source);
    if SOURCE_IN_FLIGHT[slot].swap(true, Ordering::AcqRel) {
        None
    } else {
        Some(SourceInFlightGuard { slot })
    }
}

/// Runs only the policy-permitted providers inside the bounded `ALL_SOURCES` slots
/// and returns raw fresh readings before aggregation.
fn read_all_raw(policy: &ProbePolicy) -> Option<Vec<WirelessReading>> {
    let gate = POLL_GATE.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = gate.try_lock() else {
        return None;
    };
    let devices = Arc::new(enumerate_hid_devices());
    let policy = Arc::new(policy.clone());
    let (sender, receiver) = mpsc::channel::<Vec<WirelessReading>>();
    let mut cancellations = Vec::with_capacity(ALL_SOURCES.len());
    let mut threads = Vec::with_capacity(ALL_SOURCES.len());
    for source in ALL_SOURCES {
        if !policy.allows_source(source) {
            continue;
        }
        let Some(in_flight_guard) = try_claim_source(source) else {
            continue;
        };
        let sender = sender.clone();
        let devices = Arc::clone(&devices);
        let policy = Arc::clone(&policy);
        let cancel = Arc::new(AtomicBool::new(false));
        cancellations.push(Arc::clone(&cancel));
        if let Ok(handle) = std::thread::Builder::new()
            .name(format!("ot-wb-{source:?}"))
            .spawn(move || {
                let _in_flight_guard = in_flight_guard;
                let result = catch_unwind(AssertUnwindSafe(|| {
                    run_probe(source, &devices, &cancel, &policy)
                }))
                .unwrap_or_default();
                let _ = sender.send(result);
            })
        {
            threads.push(handle);
        }
    }
    drop(sender);
    let deadline = Instant::now() + PROVIDER_TIMEOUT;
    let mut fresh = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(items) => fresh.extend(items),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => break,
        }
    }
    for cancel in &cancellations {
        cancel.store(true, Ordering::Relaxed);
    }
    // WS-4: give cancelled probes a bounded grace period, then join every finished thread.
    let join_deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < join_deadline && threads.iter().any(|handle| !handle.is_finished()) {
        std::thread::sleep(Duration::from_millis(5));
    }
    for handle in threads {
        if handle.is_finished() {
            let _ = handle.join();
        }
    }
    Some(fresh)
}

/// Tüm kablosuz pil okumalarını toplar. Engelleyici değildir: her sağlayıcı ayrı
/// iş parçacığında 1.2 sn'lik bütçeyle çalışır, zaman aşanlar atlanır.
pub fn read_all() -> Vec<WirelessReading> {
    match read_all_raw(&ProbePolicy::default()) {
        Some(fresh) => finalize(fresh),
        None => cached_snapshot(),
    }
}

// ---------------------------------------------------------------------------
// Donanım gerektirmeyen saf unit testler
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_claim_source_prevents_duplicates_and_releases_on_drop() {
        let first = try_claim_source(BatterySource::SteelSeriesGg);
        assert!(first.is_some(), "initial claim must succeed");
        assert!(
            try_claim_source(BatterySource::SteelSeriesGg).is_none(),
            "duplicate concurrent claim for the same source must be rejected"
        );
        let other = try_claim_source(BatterySource::LogitechGHub);
        assert!(
            other.is_some(),
            "different sources must be claimable concurrently"
        );
        drop(first);
        let reclaimed = try_claim_source(BatterySource::SteelSeriesGg);
        assert!(
            reclaimed.is_some(),
            "slot must be released when guard is dropped"
        );
        drop(reclaimed);
        drop(other);
    }

    #[test]
    fn live_polling_covers_every_probe_provider() {
        // The loopback providers used to exist only in `run_probe`, so they
        // could never appear in a live snapshot. Guard the complete list.
        for source in [
            BatterySource::XInput,
            BatterySource::LogitechHidPlusPlus,
            BatterySource::RazerHid,
            BatterySource::SteelSeriesHid,
            BatterySource::StandardHid,
            BatterySource::BluetoothGatt,
            BatterySource::AsusRogHid,
            BatterySource::CorsairHid,
            BatterySource::GamepadHid,
            BatterySource::HyperXHid,
            BatterySource::CompxAtkVxe,
            BatterySource::Keyboard24gHid,
            BatterySource::Bluetooth,
            BatterySource::LogitechGHub,
            BatterySource::SteelSeriesGg,
        ] {
            assert!(
                ALL_SOURCES.contains(&source),
                "{source:?} must be polled by read_all()"
            );
        }
    }

    #[test]
    fn disk_cache_path_is_the_shared_reference_file() {
        let aggregator = WirelessBatteryAggregator::new(AggregatorSettings::default());
        assert_eq!(
            aggregator.cache_path.as_deref(),
            Some(
                opentaskbar_core::paths::shared_app_data_dir()
                    .join("wireless-battery-cache.json")
                    .as_path()
            )
        );
    }

    #[test]
    fn reads_reference_bom_and_pascalcase_cache_fields() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ot-wb-reference-cache-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("wireless-battery-cache.json");
        let sampled = unix_secs_to_rfc3339(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                - 30,
        );
        let document = format!(
            "\u{feff}{{\"reference-key\":{{\"Id\":\"hid:046D:C52B\",\"Name\":\"Mouse\",\"BatteryLevel\":72,\"IsCharging\":true,\"Source\":2,\"SampledAt\":\"{sampled}\"}}}}"
        );
        std::fs::write(&path, document).expect("reference-format cache fixture must be writable");
        let mut aggregator = WirelessBatteryAggregator::new(AggregatorSettings::default())
            .with_cache_path(path.clone());
        aggregator.ensure_loaded(Instant::now());
        let cached = aggregator
            .cache
            .get("reference-key")
            .expect("BOM + PascalCase entry must load");
        assert_eq!(cached.name, "Mouse");
        assert_eq!(cached.level, 72);
        assert!(cached.charging);
        assert_eq!(cached.source, BatterySource::SteelSeriesHid);
        assert!(cached.stale);

        aggregator.save(Instant::now());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(
            saved.starts_with('\u{feff}'),
            "reference writes a UTF-8 BOM"
        );
        assert!(saved.contains("\"BatteryLevel\":72"));
        assert!(saved.contains("\"SampledAt\":"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reference_timestamp_parser_handles_offsets() {
        assert_eq!(
            reference_timestamp_secs("2026-11-27T12:34:56+00:00"),
            reference_timestamp_secs("2026-11-27T12:34:56Z")
        );
        assert_eq!(
            reference_timestamp_secs("2026-11-27T12:34:56.1234567+03:00"),
            reference_timestamp_secs("2026-11-27T09:34:56Z")
        );
        assert_eq!(reference_timestamp_secs("bad"), None);
    }

    #[test]
    fn xinput_coarse_levels_map_without_fake_percent() {
        assert_eq!(xinput_level_01(0), Some(0.25));
        assert_eq!(xinput_level_01(1), Some(0.50));
        assert_eq!(xinput_level_01(2), Some(0.75));
        assert_eq!(xinput_level_01(3), Some(1.00));
        assert_eq!(xinput_level_01(4), None);
        assert_eq!(coarse_level_text(BatterySource::XInput, 0.5), Some("Low"));
        assert_eq!(coarse_level_text(BatterySource::RazerHid, 0.5), None);
    }

    #[test]
    fn device_id_normalizes_serial_and_falls_back_to_path_hash() {
        assert_eq!(
            build_device_id("hid", 0x046D, 0xC52B, "  SN#12345  ", r"\\?\hid#ignored"),
            "hid:046D:C52B:SN12345"
        );
        let first = build_device_id("hid", 0x046D, 0xC52B, "", r"\\?\hid#one");
        let second = build_device_id("hid", 0x046D, 0xC52B, "", r"\\?\hid#two");
        assert_ne!(first, second);
        assert_eq!(
            first,
            build_device_id("hid", 0x046D, 0xC52B, "", r"\\?\hid#one")
        );
        assert!(first.starts_with("hid:046D:C52B:"));
        assert_eq!(first.len(), "hid:046D:C52B:00000000".len());
    }

    #[test]
    fn wired_device_name_filter() {
        assert!(is_ignored_wired_device(""));
        assert!(is_ignored_wired_device("  Wraith W75 Keyboard  "));
        assert!(is_ignored_wired_device("HyperX Cloud II Wired"));
        assert!(is_ignored_wired_device("Wraith W75 Wireless Headset"));
        assert!(!is_ignored_wired_device("Xbox Wireless Controller"));
        let wired = reading(
            "hid:2E3C:C368:1".into(),
            "Unknown".into(),
            0.5,
            true,
            false,
            BatterySource::StandardHid,
        );
        assert!(is_ignored_wired_reading(&wired));
    }

    #[test]
    fn standard_hid_logical_range_scaling() {
        assert_eq!(normalize_logical_to_percent(50, 0, 100), 50);
        assert_eq!(normalize_logical_to_percent(5, 0, 10), 50);
        assert_eq!(normalize_logical_to_percent(3, 1, 4), 67);
        assert_eq!(normalize_logical_to_percent(101, 0, 100), -1);
        assert_eq!(normalize_logical_to_percent(200, 0, 1000), 20);
        assert_eq!(normalize_logical_to_percent(70, 0, 10), -1);
    }

    #[test]
    fn razer_crc_checksum_validates_reply() {
        let request = razer_build_command(RAZER_CMD_BATTERY, 0x1F, false);
        assert_eq!(request.len(), RAZER_PAYLOAD_LEN);
        assert_eq!(request[88], razer_xor_crc(&request));
        let mut framed = vec![0u8; RAZER_BUFFER_LEN];
        framed[1..].copy_from_slice(&request);
        assert_eq!(razer_xor_crc(&framed), request[88]);
        let mut reply = vec![0u8; RAZER_BUFFER_LEN];
        reply[1] = RAZER_STATUS_SUCCESS;
        reply[7] = RAZER_COMMAND_CLASS_POWER;
        reply[8] = RAZER_CMD_BATTERY;
        reply[10] = 200;
        reply[89] = razer_xor_crc(&reply);
        assert_eq!(razer_parse_reply(&reply, RAZER_CMD_BATTERY), Some(200));
        reply[20] ^= 0x01;
        assert_eq!(razer_parse_reply(&reply, RAZER_CMD_BATTERY), None);
        assert!((razer_level_01(255) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn logitech_unified_and_voltage_parsing() {
        let mut reply = [0u8; 7];
        reply[0] = HIDPP_LONG_REPORT;
        reply[1] = 0xFF;
        reply[2] = 0x03;
        reply[4] = 75;
        reply[5] = 0;
        reply[6] = 0x01;
        assert_eq!(hidpp_parse_unified(&reply, 0xFF, 3), Some((0.75, true)));
        assert_eq!(hidpp_parse_status(&reply, 0xFF, 3), Some((0.75, true)));
        reply[4] = 0;
        reply[5] = 0x02;
        assert_eq!(hidpp_parse_unified(&reply, 0xFF, 3), Some((0.20, true)));
        assert_eq!(estimate_percent_from_voltage_mv(3500), 0);
        assert_eq!(estimate_percent_from_voltage_mv(4180), 100);
        assert!((estimate_percent_from_voltage_mv(3700) - 20).abs() <= 1);
    }

    #[test]
    fn steelseries_step_scaling_and_protocol_parsing() {
        assert!((ss_scale8(4) - 0.5).abs() < f32::EPSILON);
        assert!((ss_scale4(2) - 0.5).abs() < f32::EPSILON);
        assert_eq!(ss_protocol_for_pid(0x12E0), Some(SsProtocol::NovaPro));
        assert_eq!(ss_protocol_for_pid(0x2202), Some(SsProtocol::Nova7));
        assert_eq!(ss_protocol_for_pid(0x1858), Some(SsProtocol::Mouse));
        let mut reply = vec![0u8; 16];
        reply[0] = 0x06;
        reply[1] = 0xB0;
        reply[6] = 8;
        reply[7] = 4;
        reply[15] = 0x08;
        let parsed = ss_parse(SsProtocol::NovaPro, &reply).expect("valid nova pro reply");
        assert!((parsed.level - 1.0).abs() < f32::EPSILON);
        assert!(parsed.connected && !parsed.charging);
        assert_eq!(parsed.spare, Some(0.5));
    }
}

// ============================================================================
// WS-G: pure vendor report parsers + aggregator / diagnostics.
// ============================================================================

impl BatterySource {
    /// Lower value wins when two providers report the same device.
    pub fn priority(self) -> u8 {
        match self {
            Self::SteelSeriesHid => 0,
            Self::CompxAtkVxe => 1,
            Self::LogitechHidPlusPlus => 2,
            Self::RazerHid
            | Self::HyperXHid
            | Self::CorsairHid
            | Self::AsusRogHid
            | Self::Keyboard24gHid
            | Self::GamepadHid => 3,
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
    pub device_kind: WirelessDeviceKind,
}

impl BatteryReading {
    pub fn new(id: &str, name: &str, level: u32, charging: bool, source: BatterySource) -> Self {
        let trimmed = name.trim();
        let device_kind =
            super::wireless_vendors_ext::classify_wireless_device_kind(0, 0, trimmed, source, id);
        Self {
            id: id.to_owned(),
            name: trimmed.to_owned(),
            level: level.min(100) as i32,
            connected: true,
            charging,
            stale: false,
            source,
            sampled_at: Instant::now(),
            voltage_mv: None,
            secondary_level: None,
            level_override: None,
            device_kind,
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
    if reply.len() >= 7
        && reply[0] == 0
        && reply[1] == ASUS_CMD_CATEGORY
        && reply[2] == ASUS_CMD_GET_BATTERY
    {
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
        if name.trim().is_empty() {
            "ASUS ROG Wireless"
        } else {
            name
        },
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
        if name.trim().is_empty() {
            "Corsair Wireless Headset"
        } else {
            name
        },
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
        if name.trim().is_empty() {
            "HyperX Wireless"
        } else {
            name
        },
        level as u32,
        charging,
        BatterySource::HyperXHid,
    ))
}

// ---------------------------------------------------------------------------------------------
// 4. Compx / ATK / VXE / Mchose / Lamzu / Keychron ... (13+ VIDs, 17/64/65/9-byte reports)
// ---------------------------------------------------------------------------------------------

pub const COMPX_VENDORS: &[u16] = &[
    0x3554, 0x373B, 0x36A7, 0x36B0, 0x3710, 0x372E, 0x359B, 0x248A, 0x3151, 0x373E, 0x3434, 0x260D,
    0x24AE,
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
        if name.trim().is_empty() {
            "ATK Wireless Mouse"
        } else {
            name
        },
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
        if name.trim().is_empty() {
            "Wireless Gaming Mouse"
        } else {
            name
        },
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
    if r.len() < 9 || r[..6] != [0x82, 0x08, 0x02, 0x00, 0xFF, 0x03] {
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
        if name.trim().is_empty() {
            "ATK 1K Wireless Mouse"
        } else {
            name
        },
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

fn normalize_keyboard_24g_framing(raw: &[u8]) -> &[u8] {
    if raw.len() >= 4 && raw[0] == 0x00 && matches!(raw[1], 0x08 | 0x04 | 0x83) {
        &raw[1..]
    } else {
        raw
    }
}

pub fn parse_keyboard_24g(
    reply: &[u8],
    report_len: usize,
    id: &str,
    name: &str,
) -> Option<BatteryReading> {
    if report_len == COMPX_REPORT_LEN {
        return parse_compx17(reply, id, name).map(|mut r| {
            r.source = BatterySource::Keyboard24gHid;
            r.device_kind = WirelessDeviceKind::Keyboard;
            r
        });
    }
    let r = normalize_keyboard_24g_framing(reply);
    if r.len() < 3 {
        return None;
    }
    let (level, charging) = if r.len() >= 10 && r[0] == 0x08 && r[1] == COMPX_QUERY_CMD {
        if r.len() >= COMPX_REPORT_LEN && compx_checksum(r) != r[COMPX_REPORT_LEN - 1] {
            return None;
        }
        let level = r[6];
        let charge_flag = r[7];
        let voltage_mv = u16::from_be_bytes([r[8], r[9]]);
        if level > 100 || charge_flag > 1 || (level == 0 && charge_flag == 0 && voltage_mv == 0) {
            return None;
        }
        (level, charge_flag == 1)
    } else if r.len() >= 9 && r[0] == COMPX_QUERY_CMD {
        let level = r[5];
        let charge_flag = r[6];
        let voltage_mv = u16::from_be_bytes([r[7], r[8]]);
        if level > 100 || charge_flag > 1 || (level == 0 && charge_flag == 0 && voltage_mv == 0) {
            return None;
        }
        (level, charge_flag == 1)
    } else if r[0] == 0x83 {
        // Sinowealth 64-byte keyboard battery response: [0x83, chargingFlag (0/1), batteryPercent (1..100), ...]
        if r[1] > 1 {
            return None;
        }
        let charging = r[1] == 1;
        let level = r[2];
        if level == 0 || level > 100 {
            return None;
        }
        (level, charging)
    } else {
        return None;
    };
    let clean_name = if name.trim().is_empty() {
        "2.4G Wireless Keyboard"
    } else {
        name.trim()
    };
    if is_ignored_wired(0, 0, clean_name) {
        return None;
    }
    let mut reading = BatteryReading::new(
        id,
        clean_name,
        u32::from(level),
        charging,
        BatterySource::Keyboard24gHid,
    );
    reading.device_kind = WirelessDeviceKind::Keyboard;
    Some(reading)
}

pub fn is_keyboard_24g(vendor_id: u16, product_id: u16) -> bool {
    if vendor_id == 0x258A && (0x2011..=0x2022).contains(&product_id) {
        return false;
    }
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
        if name.trim().is_empty() {
            "DualSense"
        } else {
            name
        },
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
        if name.trim().is_empty() {
            "DualShock 4"
        } else {
            name
        },
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
        if name.trim().is_empty() {
            "Nintendo Switch Pro"
        } else {
            name
        },
        level,
        charging,
        BatterySource::GamepadHid,
    ))
}

pub fn parse_gamepad(
    vendor: u16,
    product: u16,
    report: &[u8],
    id: &str,
    name: &str,
) -> Option<BatteryReading> {
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
    lower.contains("127.0.0.1") || lower.contains("localhost") || lower.starts_with("http://[::1]")
}

/// Parses G-Hub loopback JSON into readings (`ParseGHubJson` / `ExtractGHubDevices`).
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

fn extract_loopback_devices(
    node: &serde_json::Value,
    source: BatterySource,
    out: &mut Vec<BatteryReading>,
) {
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
                    if lower.contains("endpoint")
                        && lower.contains("gc")
                        && let Some(url) = v.as_str()
                    {
                        return Some(url.to_owned());
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
        if name.trim().is_empty() {
            "SteelSeries Wireless"
        } else {
            name
        },
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
        if name.trim().is_empty() {
            "BLE Device"
        } else {
            name
        },
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
        let collision =
            !n.is_empty() && counts.get(&(r.source, n.clone())).copied().unwrap_or(0) > 1;
        let key = if !n.is_empty() && !collision {
            format!("name:{n}")
        } else if !r.id.trim().is_empty() {
            format!(
                "src:{}:id:{}",
                r.source as u8,
                r.id.trim().to_ascii_lowercase()
            )
        } else {
            format!("name:{n}")
        };
        match by_key.get_mut(&key) {
            Some(existing) if ws_g_prefer(&r, existing) => {
                let mut preferred = r;
                merge_metadata(&mut preferred, existing);
                *existing = preferred;
            }
            Some(existing) => merge_metadata(existing, &r),
            None => {
                by_key.insert(key, r);
            }
        }
    }
    by_key.into_values().collect()
}

fn ws_g_prefer(candidate: &BatteryReading, existing: &BatteryReading) -> bool {
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
    if primary.device_kind == WirelessDeviceKind::Other
        && secondary.device_kind != WirelessDeviceKind::Other
    {
        primary.device_kind = secondary.device_kind;
    }
}

#[derive(Clone, Default)]
pub struct OutlierGuard {
    pending: HashMap<String, i32>,
}

/// The guard is process-wide: a fresh aggregator is built on every poll, but
/// the "one poll of doubt" state must survive between those polls or the
/// >=25-point drop protection is silently bypassed on every second sample.
static PERSISTENT_GUARD: OnceLock<Mutex<OutlierGuard>> = OnceLock::new();

/// Access to the live outlier guard (also used by tests to verify persistence).
pub fn persistent_guard() -> MutexGuard<'static, OutlierGuard> {
    PERSISTENT_GUARD
        .get_or_init(|| Mutex::new(OutlierGuard::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    pub multi_brand_enabled: bool,
    pub disabled_vendors: Vec<String>,
}

impl Default for AggregatorSettings {
    fn default() -> Self {
        Self {
            poll_seconds: 30,
            low_threshold: 20,
            ignored_devices: Vec::new(),
            aliases: HashMap::new(),
            listen_only: false,
            multi_brand_enabled: true,
            disabled_vendors: Vec::new(),
        }
    }
}

impl AggregatorSettings {
    /// `validate()` range for `WirelessBatteryPollSeconds` is 15..=300.
    pub fn poll_seconds_clamped(&self) -> u32 {
        self.poll_seconds.clamp(15, 300)
    }

    pub fn is_ignored(&self, reading: &BatteryReading) -> bool {
        Self::is_ignored_fields(&self.ignored_devices, &reading.id, &reading.name)
    }

    pub fn display_name(&self, reading: &BatteryReading) -> String {
        Self::display_name_fields(&self.aliases, &reading.id, &reading.name)
    }

    pub fn allows(&self, source: BatterySource) -> bool {
        ProbePolicy::from(self).allows_source(source)
    }

    /// Applies the same ignore / alias / listen-only / multi-brand / vendor policy to live readings.
    pub fn allows_reading(&self, reading: &WirelessReading) -> bool {
        !Self::is_ignored_fields(&self.ignored_devices, &reading.id, &reading.name)
            && self.allows(reading.source)
    }

    pub fn display_name_of(&self, reading: &WirelessReading) -> String {
        Self::display_name_fields(&self.aliases, &reading.id, &reading.name)
    }

    fn is_ignored_fields(ignored: &[String], id: &str, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        let id = id.to_ascii_lowercase();
        ignored.iter().any(|x| {
            !x.is_empty()
                && (name.contains(&x.to_ascii_lowercase()) || id == x.to_ascii_lowercase())
        })
    }

    fn display_name_fields(aliases: &HashMap<String, String>, id: &str, name: &str) -> String {
        aliases
            .get(&id.to_ascii_lowercase())
            .cloned()
            .unwrap_or_else(|| name.to_owned())
    }
}

impl From<&DockSettings> for AggregatorSettings {
    fn from(settings: &DockSettings) -> Self {
        Self {
            poll_seconds: settings.wireless_battery_poll_seconds.max(0) as u32,
            low_threshold: settings.wireless_battery_low_threshold.max(0) as u32,
            ignored_devices: settings.ignored_wireless_devices.clone(),
            aliases: settings
                .wireless_device_aliases
                .iter()
                .map(|(id, alias)| (id.to_ascii_lowercase(), alias.clone()))
                .collect(),
            listen_only: settings.wireless_battery_listen_only,
            multi_brand_enabled: settings.wireless_multi_brand_enabled(),
            disabled_vendors: settings.wireless_disabled_vendors(),
        }
    }
}

/// Converts a live transport reading into the aggregator's richer type.
fn to_battery_reading(item: &WirelessReading) -> BatteryReading {
    let level = (item.level_01 * 100.0).round().clamp(0.0, 100.0) as u32;
    let mut reading =
        BatteryReading::new(&item.id, &item.name, level, item.is_charging, item.source);
    reading.connected = item.is_connected;
    reading.stale = item.stale;
    reading.device_kind = item.device_kind;
    reading.secondary_level = item
        .secondary_level
        .map(|v| (v * 100.0).round().clamp(0.0, 100.0) as i32);
    reading
}

fn to_wireless_reading(item: BatteryReading) -> WirelessReading {
    let secondary_level = item
        .secondary_level
        .filter(|&v| (0..=100).contains(&v))
        .map(percent_01);
    let mut wireless = reading_with_meta(
        item.id,
        item.name,
        percent_01(item.level),
        item.connected,
        item.charging,
        item.source,
        item.device_kind,
        secondary_level,
    );
    wireless.stale = item.stale;
    wireless
}

static AGG_GATE: OnceLock<Mutex<()>> = OnceLock::new();
static LAST_AGGREGATE: Mutex<Option<(Instant, Vec<WirelessReading>)>> = Mutex::new(None);

fn cached_aggregate() -> Option<Vec<WirelessReading>> {
    LAST_AGGREGATE
        .lock()
        .ok()?
        .as_ref()
        .map(|(_, readings)| readings.clone())
}

/// Live transports + the real aggregator (outlier guard, 30-minute stale
/// retention, ignore/alias policy and the on-disk JSON cache). Enforces
/// `listen_only`, `wireless_multi_brand_enabled()`, and `wireless_disabled_vendors()`
/// BEFORE opening/writing any HID device, then merges cleanly through
/// [`WirelessBatteryAggregator`].
pub fn read_all_with_settings(settings: &DockSettings) -> Vec<WirelessReading> {
    let agg_settings = AggregatorSettings::from(settings);
    let interval = Duration::from_secs(agg_settings.poll_seconds_clamped() as u64);

    if let Ok(last) = LAST_AGGREGATE.lock()
        && let Some((sampled_at, readings)) = last.as_ref()
        && sampled_at.elapsed() < interval
    {
        return readings.clone();
    }

    let gate = AGG_GATE.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = gate.try_lock() else {
        return cached_aggregate().unwrap_or_default();
    };
    // Another poll may have completed while this caller waited for the gate.
    if let Ok(last) = LAST_AGGREGATE.lock()
        && let Some((sampled_at, readings)) = last.as_ref()
        && sampled_at.elapsed() < interval
    {
        return readings.clone();
    }

    let policy = ProbePolicy::from(&agg_settings);
    let Some(raw_fresh) = read_all_raw(&policy) else {
        return cached_aggregate().unwrap_or_else(cached_snapshot);
    };
    let fresh: Vec<BatteryReading> = raw_fresh
        .iter()
        .filter(|r| !is_ignored_wired_reading(r))
        .map(to_battery_reading)
        .collect();
    let mut aggregator = WirelessBatteryAggregator::new(agg_settings);
    // The aggregator object is rebuilt per poll; the guard is not. Carry the
    // pending-drop state in and write it back so consecutive drops are detected.
    aggregator.guard = persistent_guard().clone();
    let merged = aggregator
        .merge(fresh)
        .into_iter()
        .map(to_wireless_reading)
        .collect::<Vec<_>>();
    *persistent_guard() = aggregator.guard.clone();

    if let Ok(mut last) = LAST_AGGREGATE.lock() {
        *last = Some((Instant::now(), merged.clone()));
    }
    merged
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
        // Same file name and folder as `WirelessBatteryAggregator`.
        let cache_path = Some(
            opentaskbar_core::paths::shared_app_data_dir().join("wireless-battery-cache.json"),
        );
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

    /// reference writes JSON with `Encoding.UTF8`, which emits a BOM. other readers
    /// strip it, so the Rust reader has to tolerate both forms.
    fn cache_json(raw: &str) -> Option<serde_json::Value> {
        serde_json::from_str(raw.strip_prefix('\u{feff}').unwrap_or(raw)).ok()
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
        let Some(root) = Self::cache_json(&json) else {
            return;
        };
        let Some(map) = root.as_object() else {
            return;
        };
        // Absolute write time is authoritative. A clock moving backwards must
        // never produce a future-dated (or negative-age) cache entry.
        let wall_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        for (key, value) in map {
            if self.cache.len() >= 64 {
                break;
            }
            let level = cache_field(value, "level", "BatteryLevel")
                .and_then(|x| x.as_i64())
                .unwrap_or(-1) as i32;
            if !(-1..=100).contains(&level) {
                continue;
            }
            let source_id = cache_field(value, "source", "Source")
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u8;
            let age_secs = if let Some(written) =
                cache_field(value, "written_at", "WrittenAt").and_then(|x| x.as_u64())
            {
                wall_now.saturating_sub(written)
            } else if let Some(sampled) = value
                .get("SampledAt")
                .and_then(|x| x.as_str())
                .and_then(reference_timestamp_secs)
            {
                wall_now.saturating_sub(sampled)
            } else {
                cache_field(value, "age_secs", "AgeSecs")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0)
            };
            if Duration::from_secs(age_secs) > STALE_WINDOW {
                continue;
            }
            let mut reading = BatteryReading::new(
                cache_field(value, "id", "Id")
                    .and_then(|x| x.as_str())
                    .unwrap_or(key),
                cache_field(value, "name", "Name")
                    .and_then(|x| x.as_str())
                    .unwrap_or(""),
                level.max(0) as u32,
                cache_field(value, "charging", "IsCharging")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false),
                source_from_id(source_id),
            );
            if level < 0 {
                // A cached -1 means "level unknown"; promote it to an explicit
                // override instead of presenting a false 0% reading to the UI.
                reading.level = -1;
                let override_text = cache_field(value, "level_override", "LevelOverrideText")
                    .and_then(|x| x.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("-1");
                reading.level_override = Some(override_text.to_owned());
            }
            if let Some(spare) = cache_field(value, "secondary_level", "SecondaryBatteryLevel")
                .and_then(|x| x.as_i64())
                .filter(|&v| (0..=100).contains(&v))
            {
                reading.secondary_level = Some(spare as i32);
            }
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
        let wall_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut root = serde_json::Map::new();
        for (key, r) in &self.cache {
            let age_secs = now.saturating_duration_since(r.sampled_at).as_secs();
            let sampled = wall_now.saturating_sub(age_secs.min(wall_now));
            // Both spellings are written: snake_case for this port's existing
            // readers, PascalCase for `WirelessDeviceReading` deserialization.
            root.insert(
                key.clone(),
                serde_json::json!({
                    "id": r.id,
                    "name": r.name,
                    "level": r.level,
                    "charging": r.charging,
                    "source": r.source as u8,
                    "age_secs": age_secs,
                    "written_at": wall_now,
                    "secondary_level": r.secondary_level,
                    "Id": r.id,
                    "Name": r.name,
                    "BatteryLevel": r.level,
                    "IsCharging": r.charging,
                    "Source": r.source as u8,
                    "SampledAt": unix_secs_to_rfc3339(sampled),
                    "LevelOverrideText": r.level_override,
                    "SecondaryBatteryLevel": r.secondary_level,
                }),
            );
        }
        let json = serde_json::to_string(&serde_json::Value::Object(root)).unwrap_or_default();
        // Write to a sibling temporary file then rename: readers never observe a
        // half-written JSON document when the dock restarts mid-save.
        let temp = path.with_extension("json.tmp");
        // `File.WriteAllText(..., Encoding.UTF8)` emits a UTF-8 BOM.
        let with_bom = format!("\u{feff}{json}");
        if std::fs::write(&temp, with_bom.as_bytes()).is_err() {
            let _ = std::fs::remove_file(&temp);
            return;
        }
        if std::fs::rename(&temp, path).is_err() {
            let _ = std::fs::remove_file(&temp);
        }
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
            let key = format!(
                "src:{}:id:{}",
                reading.source as u8,
                reading.id.to_ascii_lowercase()
            );
            active.insert(key.clone());
            reading.name = self.settings.display_name(&reading);
            // An explicit -1 override means the level is unknown; never feed it
            // through the drop guard or present it as a real percentage.
            if reading.connected && reading.level >= 0 && reading.level_override.is_none() {
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
        // Deterministic source-priority ordering: freshest, charging devices
        // from the most trustworthy transports first.
        output.sort_by(|a, b| {
            a.stale
                .cmp(&b.stale)
                .then_with(|| b.charging.cmp(&a.charging))
                .then_with(|| a.source.priority().cmp(&b.source.priority()))
                .then_with(|| a.level.cmp(&b.level))
                .then_with(|| {
                    a.name
                        .to_ascii_lowercase()
                        .cmp(&b.name.to_ascii_lowercase())
                })
        });
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
pub fn probe_report(descriptor: &HidInterfaceDescriptor, report: &[u8]) -> Option<BatteryReading> {
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
        return parse_gamepad(
            descriptor.vendor_id,
            descriptor.product_id,
            report,
            &id,
            name,
        );
    }
    if descriptor.vendor_id == 0x1038 {
        return parse_steelseries_hid(report, &id, name);
    }
    None
}

#[cfg(test)]
mod ws_g_tests {
    use super::*;

    #[test]
    fn ws4_loopback_endpoints_are_strictly_local() {
        assert_eq!(
            parse_loopback_endpoint("ws://127.0.0.1:9010", 9010, true).as_deref(),
            Some("127.0.0.1:9010")
        );
        assert_eq!(
            parse_loopback_endpoint("http://localhost:57342", 57342, false).as_deref(),
            Some("localhost:57342")
        );
        assert!(parse_loopback_endpoint("ws://evil.example.com:9010", 9010, true).is_none());
        assert!(parse_loopback_endpoint("http://127.0.0.1:9011", 9010, false).is_none());
        assert_eq!(url_port("http://127.0.0.1:57342/battery"), Some(57342));
    }

    #[test]
    fn ws4_ghub_and_steelseries_parsers_feed_aggregator_shape() {
        let ghub = r#"{"devices":[{"name":"G Pro","id":"mouse-1","battery":75,"charging":true}]}"#;
        let parsed = parse_ghub_json(ghub);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].level, 75);
        let wireless = reading(
            format!("ghub:{}", parsed[0].id),
            parsed[0].name.clone(),
            percent_01(parsed[0].level),
            parsed[0].connected,
            parsed[0].charging,
            parsed[0].source,
        );
        assert_eq!(wireless.level_percent(), 75);
        assert_eq!(wireless.source, BatterySource::LogitechGHub);

        let gg = r#"{"batteryLevel":42,"deviceName":"Arctis"}"#;
        let parsed = parse_steelseries_gg_json(gg);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].level, 42);
    }

    #[test]
    fn ws4_aggregator_converts_live_readings_and_honours_poll_interval() {
        let dock = opentaskbar_core::settings::DockSettings {
            wireless_battery_poll_seconds: 15,
            ..Default::default()
        };
        let agg = AggregatorSettings::from(&dock);
        let live = reading(
            "bt-pnp:1".into(),
            "Headset".into(),
            0.8,
            true,
            false,
            BatterySource::Bluetooth,
        );
        let battery = to_battery_reading(&live);
        assert_eq!(battery.level, 80);
        let restored = to_wireless_reading(battery);
        assert_eq!(restored.level_percent(), 80);
        assert_eq!(restored.source, BatterySource::Bluetooth);
        assert_eq!(agg.poll_seconds_clamped(), 15);
        // The cache gate is keyed by the setting; no hardware is needed here.
        assert!(cached_aggregate().is_none_or(|items| items.len() <= 64));
    }

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
        for (page, _prefix) in [
            (0xFF00, 0x03),
            (0xFF13, 0x05),
            (0xFF90, 0x06),
            (0xFFC0, 0xC0),
        ] {
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
        let mut reply = request;
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
        let json =
            r#"{"devices":[{"name":"G Pro X","batteryLevel":73,"charging":false,"id":"1"}]}"#;
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
    fn outlier_guard_survives_a_fresh_aggregator() {
        let key = "ws4-persistent-guard";
        persistent_guard().pending.remove(key);
        let (level, guarded) = persistent_guard().check(key, 90, 60);
        assert_eq!(level, 90);
        assert!(guarded, "first implausible drop must be held");

        // A new aggregator (as built on every poll) must inherit the doubt.
        let mut aggregator = WirelessBatteryAggregator::new(AggregatorSettings::default());
        aggregator.guard = persistent_guard().clone();
        let (level, guarded) = aggregator.guard.check(key, 90, 55);
        assert_eq!(level, 55);
        assert!(!guarded, "second consecutive drop must be accepted");
        *persistent_guard() = aggregator.guard;
        persistent_guard().pending.remove(key);
    }

    #[test]
    fn disk_cache_handles_future_skew_and_minus_one_override() {
        let dir = std::env::temp_dir().join(format!("otb-wb-cache-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("cache.json");
        let wall_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let document = serde_json::json!({
            "unknown": {
                "id": "unknown", "name": "Unknown", "level": -1,
                "charging": false, "source": 0, "age_secs": 0,
                "written_at": wall_now + 600
            },
            "fresh": {
                "id": "fresh", "name": "Headset", "level": 80,
                "charging": true, "source": 2, "age_secs": 0,
                "written_at": wall_now
            }
        });
        std::fs::write(&path, document.to_string()).unwrap();
        let mut aggregator = WirelessBatteryAggregator::new(AggregatorSettings::default())
            .with_cache_path(path.clone());
        aggregator.ensure_loaded(Instant::now());
        let unknown = &aggregator.cache["unknown"];
        assert_eq!(unknown.level, -1);
        assert_eq!(unknown.level_override.as_deref(), Some("-1"));
        assert!(unknown.stale);
        assert_eq!(aggregator.cache["fresh"].level, 80);

        aggregator.save(Instant::now());
        assert!(path.is_file());
        assert!(!path.with_extension("json.tmp").exists());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("\"written_at\""));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn aggregator_settings_privacy_and_merge() {
        let mut settings = AggregatorSettings {
            poll_seconds: 999,
            ..Default::default()
        };
        settings.aliases.insert("id1".into(), "My Mouse".into());
        settings.ignored_devices.push("hidden".into());
        assert_eq!(settings.poll_seconds_clamped(), 300);
        let mut hidden =
            BatteryReading::new("hidden", "Hidden", 50, false, BatterySource::Bluetooth);
        let mut good = BatteryReading::new("id1", "Mouse", 80, false, BatterySource::AsusRogHid);
        assert!(settings.is_ignored(&hidden));
        assert_eq!(settings.display_name(&good), "My Mouse");
        hidden.name = "Hidden".into();
        let desc = [
            desc(0x2E3C, 0xC368, 0xFF00, 64),
            desc(0x0B05, 1, 0xFF00, 64),
        ];
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

    #[test]
    fn aggregator_settings_from_dock_settings() {
        let mut dock = opentaskbar_core::settings::DockSettings {
            wireless_battery_poll_seconds: 45,
            wireless_battery_low_threshold: 30,
            ..Default::default()
        };
        dock.ignored_wireless_devices.push("hidden".into());
        dock.wireless_device_aliases
            .insert("ID1".into(), "My Headset".into());
        dock.wireless_battery_listen_only = true;

        let agg = super::AggregatorSettings::from(&dock);
        assert_eq!(agg.poll_seconds, 45);
        assert_eq!(agg.poll_seconds_clamped(), 45);
        assert_eq!(agg.low_threshold, 30);
        assert!(agg.listen_only);
        let active = super::WirelessReading {
            id: "id1".into(),
            name: "Headset".into(),
            level_01: 0.5,
            is_connected: true,
            is_charging: false,
            source: BatterySource::AsusRogHid,
            stale: false,
            device_kind: WirelessDeviceKind::Headphones,
            secondary_level: None,
        };
        assert!(!agg.allows_reading(&active)); // listen-only excludes active HID probes
        assert_eq!(agg.display_name_of(&active), "My Headset");
        let hidden = super::WirelessReading {
            name: "Hidden Device".into(),
            ..active.clone()
        };
        assert!(!agg.allows_reading(&hidden));
        let passive = super::WirelessReading {
            source: BatterySource::Bluetooth,
            ..active
        };
        assert!(agg.allows_reading(&passive));
    }

    #[test]
    fn keyboard_24g_sinowealth_0x83_and_checksum_validation() {
        // Sinowealth 64-byte response: [0x83, charging=1, level=77]
        let mut sino = vec![0u8; 64];
        sino[0] = 0x83;
        sino[1] = 1;
        sino[2] = 77;
        let r = parse_keyboard_24g(&sino, 64, "kbd:aula", "AULA F75").expect("Sinowealth 0x83");
        assert_eq!(r.level, 77);
        assert!(r.charging);
        assert_eq!(r.source, BatterySource::Keyboard24gHid);
        assert_eq!(r.device_kind, WirelessDeviceKind::Keyboard);

        // Leading 0x00 framed Sinowealth 0x83
        let mut framed = vec![0u8; 65];
        framed[1] = 0x83;
        framed[2] = 0;
        framed[3] = 52;
        let r2 = parse_keyboard_24g(&framed, 64, "kbd:rk", "Royal Kludge")
            .expect("framed Sinowealth 0x83");
        assert_eq!(r2.level, 52);
        assert!(!r2.charging);

        // Invalid Sinowealth (0% or >100% or invalid charging flag)
        sino[2] = 0;
        assert!(parse_keyboard_24g(&sino, 64, "kbd:bad", "AULA").is_none());
        sino[2] = 101;
        assert!(parse_keyboard_24g(&sino, 64, "kbd:bad", "AULA").is_none());
        sino[1] = 2;
        sino[2] = 50;
        assert!(parse_keyboard_24g(&sino, 64, "kbd:bad", "AULA").is_none());

        // Glorious mouse PIDs on 0x258A must NOT be classified as 2.4G keyboards
        assert!(!is_keyboard_24g(0x258A, 0x2016));
        assert!(is_keyboard_24g(0x258A, 0x010C));
    }

    #[test]
    fn steelseries_expanded_pids_and_spare_dock_battery_roundtrip() {
        assert_eq!(ss_protocol_for_pid(0x12E2), Some(SsProtocol::NovaPro));
        assert_eq!(ss_protocol_for_pid(0x12E6), Some(SsProtocol::NovaPro));
        assert_eq!(ss_protocol_for_pid(0x2240), Some(SsProtocol::Nova5));
        assert_eq!(ss_protocol_for_pid(0x162C), Some(SsProtocol::Keyboard));
        assert_eq!(ss_protocol_for_pid(0x1854), Some(SsProtocol::Mouse));
        assert_eq!(
            ss_device_kind(SsProtocol::Keyboard),
            WirelessDeviceKind::Keyboard
        );
        assert_eq!(
            ss_device_kind(SsProtocol::NovaPro),
            WirelessDeviceKind::Headphones
        );

        let live = reading_with_meta(
            "steelseries:1038:12E0:ABC".into(),
            "Arctis Nova Pro Wireless".into(),
            0.75,
            true,
            false,
            BatterySource::SteelSeriesHid,
            WirelessDeviceKind::Headphones,
            Some(0.50),
        );
        let br = to_battery_reading(&live);
        assert_eq!(br.secondary_level, Some(50));
        assert_eq!(br.device_kind, WirelessDeviceKind::Headphones);

        let cache_path =
            std::env::temp_dir().join(format!("otb-ss-spare-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&cache_path);
        let mut agg = WirelessBatteryAggregator::new(AggregatorSettings::default())
            .with_cache_path(cache_path.clone());
        let merged = agg.merge(vec![br]);
        assert_eq!(merged.len(), 1);
        let restored = to_wireless_reading(merged[0].clone());
        assert_eq!(restored.secondary_level, Some(0.50));
        assert_eq!(restored.device_kind, WirelessDeviceKind::Headphones);
        let _ = std::fs::remove_file(cache_path);
    }

    #[test]
    fn dualsense_and_ds4_bluetooth_reports_parse() {
        // DualSense BT 0x31 report (battery at byte 54)
        let mut ds_bt = vec![0u8; 78];
        ds_bt[0] = 0x31;
        ds_bt[54] = 0x07; // discharging, 70%
        let parsed = parse_gamepad(0x054C, 0x0DF2, &ds_bt, "gp:edge", "DualSense Edge")
            .expect("DualSense BT 0x31");
        assert_eq!(parsed.level, 70);
        assert!(!parsed.charging);
        assert_eq!(parsed.device_kind, WirelessDeviceKind::Gamepad);

        // DualShock 4 BT 0x11 report (battery at byte 32)
        let mut ds4_bt = vec![0u8; 78];
        ds4_bt[0] = 0x11;
        ds4_bt[32] = 0x16; // cable + step 6 -> 60% charging
        let parsed4 = parse_gamepad(0x054C, 0x09CC, &ds4_bt, "gp:ds4", "DualShock 4")
            .expect("DualShock 4 BT 0x11");
        assert_eq!(parsed4.level, 60);
        assert!(parsed4.charging);
        assert_eq!(parsed4.device_kind, WirelessDeviceKind::Gamepad);
    }

    #[test]
    fn corsair_bragi_both_endpoints_and_probe_policy_gating() {
        let req08 = build_bragi_request(0x08, BRAGI_PROPERTY_LEVEL, 64);
        assert_eq!(req08[0], 0x08);
        assert_eq!(&req08[1..4], &[0x02, 0x0F, 0x00]);
        let req09 = build_bragi_request(0x09, BRAGI_PROPERTY_STATUS, 65);
        assert_eq!(req09[0], 0x00);
        assert_eq!(req09[1], 0x09);
        assert_eq!(&req09[2..5], &[0x02, 0x10, 0x00]);

        let listen_only_policy = ProbePolicy {
            listen_only: true,
            multi_brand_enabled: true,
            disabled_vendors: Vec::new(),
        };
        assert!(!listen_only_policy.allows_source(BatterySource::AsusRogHid));
        assert!(!listen_only_policy.allows_source(BatterySource::CorsairHid));
        assert!(!listen_only_policy.allows_source(BatterySource::HyperXHid));
        assert!(!listen_only_policy.allows_source(BatterySource::CompxAtkVxe));
        assert!(!listen_only_policy.allows_source(BatterySource::Keyboard24gHid));
        assert!(!listen_only_policy.allows_source(BatterySource::RazerHid));
        assert!(listen_only_policy.allows_source(BatterySource::GamepadHid));
        assert!(listen_only_policy.allows_source(BatterySource::Bluetooth));

        let no_multi_brand = ProbePolicy {
            listen_only: false,
            multi_brand_enabled: false,
            disabled_vendors: Vec::new(),
        };
        assert!(!no_multi_brand.allows_source(BatterySource::AsusRogHid));
        assert!(!no_multi_brand.allows_source(BatterySource::CompxAtkVxe));
        assert!(no_multi_brand.allows_source(BatterySource::LogitechHidPlusPlus));

        let disabled_vendor = ProbePolicy {
            listen_only: false,
            multi_brand_enabled: true,
            disabled_vendors: vec!["Corsair".into(), "glorious".into(), "zowie".into()],
        };
        assert!(!disabled_vendor.allows_source(BatterySource::CorsairHid));
        assert!(disabled_vendor.allows_source(BatterySource::AsusRogHid));
        assert!(disabled_vendor.is_vid_disabled(0x258A, "Glorious Model O 2"));
        assert!(disabled_vendor.is_vid_disabled(0x3057, "ZOWIE U2"));
        assert!(!disabled_vendor.is_vid_disabled(0x372E, "Lamzu Maya"));
    }

    #[test]
    fn razer_reply_tx_id_distinguishes_hyperspeed_slots() {
        let mut reply_a = vec![0u8; RAZER_BUFFER_LEN];
        reply_a[1] = RAZER_STATUS_SUCCESS;
        reply_a[2] = 0x1F;
        reply_a[7] = RAZER_COMMAND_CLASS_POWER;
        reply_a[8] = RAZER_CMD_BATTERY;
        reply_a[10] = 180;
        reply_a[89] = razer_xor_crc(&reply_a);
        assert_eq!(razer_reply_tx_id(&reply_a), Some(0x1F));
        assert_eq!(razer_parse_reply(&reply_a, RAZER_CMD_BATTERY), Some(180));

        let mut reply_b = reply_a.clone();
        reply_b[2] = 0x3F;
        reply_b[10] = 90;
        reply_b[89] = razer_xor_crc(&reply_b);
        assert_eq!(razer_reply_tx_id(&reply_b), Some(0x3F));
        assert_eq!(razer_parse_reply(&reply_b, RAZER_CMD_BATTERY), Some(90));
    }
}
