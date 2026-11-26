//! Kablosuz/Bluetooth pil veri katmanı (C# `Services/Hardware/WirelessBattery` portu).
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
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

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
}

impl BatterySource {
    /// Widget etiketi (C# `WirelessDeviceReading.GetSourceLabel` karşılığı).
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
        }
    }
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
}

impl WirelessReading {
    pub fn level_percent(&self) -> u8 {
        (self.level_01 * 100.0).round().clamp(0.0, 100.0) as u8
    }
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
const DIGCF_DEVICEINTERFACE: u32 = 0x10;
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
    fn LoadLibraryW(lp_lib_file_name: *const u16) -> Handle;
    fn GetProcAddress(h_module: Handle, lp_proc_name: *const u8) -> *const c_void;
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

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

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
    WirelessReading {
        id,
        name,
        level_01: clamp01(level_01),
        is_connected,
        is_charging,
        source,
        stale: false,
    }
}

fn utf16_from_bytes(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
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
        let mut hash = 0x811C_9DC5u32;
        for byte in path.bytes() {
            hash ^= u32::from(byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
        clean = format!("{hash:08X}");
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

/// Kablolu cihaz filtresi (C# `IsIgnoredWiredDevice` + genel "wired" kontrolü).
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
    match source {
        BatterySource::SteelSeriesHid => 0,
        BatterySource::CompxAtkVxe => 1,
        BatterySource::LogitechHidPlusPlus => 2,
        BatterySource::RazerHid => 3,
        BatterySource::HyperXHid => 3,
        BatterySource::CorsairHid => 3,
        BatterySource::AsusRogHid => 3,
        BatterySource::Keyboard24gHid => 3,
        BatterySource::GamepadHid => 3,
        BatterySource::StandardHid => 4,
        BatterySource::BluetoothGatt => 6,
        BatterySource::XInput => 7,
    }
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
    let mut packet = vec![0u8; report_len];
    packet[0] = if report_len == 7 {
        HIDPP_SHORT_REPORT
    } else {
        HIDPP_LONG_REPORT
    };
    packet[1] = device_index;
    packet[2] = 0x00;
    packet[3] = HIDPP_SOFTWARE_ID & 0x0F;
    packet[4] = (feature >> 8) as u8;
    packet[5] = feature as u8;
    packet
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
}

struct SsParsed {
    level: f32,
    charging: bool,
    connected: bool,
    spare: Option<f32>,
}

fn ss_protocol_for_pid(pid: u16) -> Option<SsProtocol> {
    match pid {
        0x12E0 | 0x12E5 => Some(SsProtocol::NovaPro),
        0x2202 | 0x2206 | 0x220A | 0x223A | 0x2258 | 0x227E | 0x2266 | 0x226A | 0x220E | 0x2212
        | 0x2216 => Some(SsProtocol::Nova7),
        0x2232 | 0x2236 | 0x223E => Some(SsProtocol::Nova5),
        0x12B3 | 0x12B6 | 0x1260 | 0x12AD | 0x12D5 | 0x12D7 | 0x12C2 => {
            Some(SsProtocol::ArctisLegacy)
        }
        0x1290 => Some(SsProtocol::ArctisPro),
        0x1838 | 0x1852 | 0x1858 | 0x185E | 0x1842 | 0x1848 | 0x1830 | 0x172B => {
            Some(SsProtocol::Mouse)
        }
        _ => None,
    }
}

fn ss_default_name(pid: u16) -> &'static str {
    match pid {
        0x12E0 | 0x12E5 => "Arctis Nova Pro Wireless",
        0x2202 | 0x2206 | 0x220A | 0x223A | 0x2258 | 0x227E => "Arctis Nova 7 Wireless",
        0x2266 => "Arctis Nova 3P Wireless",
        0x226A => "Arctis Nova 3X Wireless",
        0x2232 | 0x2236 | 0x223E => "Arctis Nova 5 Wireless",
        0x12B3 | 0x12B6 => "Arctis 1 Wireless",
        0x220E | 0x2212 | 0x2216 => "Arctis 7+ Wireless",
        0x1260 | 0x12AD | 0x12D5 | 0x12D7 => "Arctis 7 Wireless",
        0x12C2 => "Arctis 9 Wireless",
        0x1290 => "Arctis Pro Wireless",
        0x1838 | 0x1852 => "Aerox 3 Wireless",
        0x1858 => "Aerox 5 Wireless",
        0x185E => "Aerox 9 Wireless",
        0x1842 => "Prime Wireless",
        0x1848 => "Prime Mini Wireless",
        0x1830 => "Rival 3 Wireless",
        0x172B => "Rival 650 Wireless",
        _ => "SteelSeries Wireless",
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
        SsProtocol::Mouse => buffer[offset] = 0xD2,
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
        SsProtocol::Mouse => payload.len() >= 3 && payload[0] == 0xD2,
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
        SsProtocol::Mouse => {
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
    let address = unsafe { GetProcAddress(module, symbol.as_ptr()) };
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
        if !filter(device) || is_ignored_wired_device(&device.product) {
            continue;
        }
        if !seen.insert((device.vid, device.pid)) {
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
        result.push(reading(
            device.id(prefix),
            name,
            level,
            true,
            charging,
            source,
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
            let mut level = None;
            let charging: bool;
            if let Some(index) = hidpp_resolve_feature(
                &session,
                device_index,
                HIDPP_FEATURE_UNIFIED_BATTERY,
                report_len,
                &budget,
                cancel,
            ) {
                let request =
                    hidpp_feature_request(device_index, index, 0x01, &[0, 0, 0], report_len);
                let mut response = vec![0u8; response_len];
                if let Some(len) = hidpp_transact(
                    &session,
                    &request,
                    &mut response,
                    device_index,
                    index,
                    &budget,
                    cancel,
                ) {
                    level = hidpp_parse_unified(&response[..len], device_index, index);
                }
            }
            if level.is_none()
                && let Some(index) = hidpp_resolve_feature(
                    &session,
                    device_index,
                    HIDPP_FEATURE_BATTERY_STATUS,
                    report_len,
                    &budget,
                    cancel,
                )
            {
                let request =
                    hidpp_feature_request(device_index, index, 0x00, &[0, 0, 0], report_len);
                let mut response = vec![0u8; response_len];
                if let Some(len) = hidpp_transact(
                    &session,
                    &request,
                    &mut response,
                    device_index,
                    index,
                    &budget,
                    cancel,
                ) {
                    level = hidpp_parse_status(&response[..len], device_index, index);
                }
            }
            if level.is_none()
                && let Some(index) = hidpp_resolve_feature(
                    &session,
                    device_index,
                    HIDPP_FEATURE_BATTERY_VOLTAGE,
                    report_len,
                    &budget,
                    cancel,
                )
            {
                let request =
                    hidpp_feature_request(device_index, index, 0x00, &[0, 0, 0], report_len);
                let mut response = vec![0u8; response_len];
                if let Some(len) = hidpp_transact(
                    &session,
                    &request,
                    &mut response,
                    device_index,
                    index,
                    &budget,
                    cancel,
                ) && let Some((percent, is_charging, _)) =
                    hidpp_parse_voltage(&response[..len], device_index, index)
                {
                    level = Some((percent, is_charging));
                }
            }
            if let Some((percent, is_charging)) = level {
                charging = is_charging;
                let name = if device.product.trim().is_empty() {
                    "Logitech Wireless".to_owned()
                } else {
                    device.product.trim().to_owned()
                };
                let mut id = device.id("logitech");
                id.push_str(&format!(":{device_index:02X}"));
                result.push(reading(
                    id,
                    name,
                    percent,
                    true,
                    charging,
                    BatterySource::LogitechHidPlusPlus,
                ));
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Sağlayıcı 4: Razer 90 bayt feature report
// ---------------------------------------------------------------------------

fn probe_razer(devices: &[HidDevice], cancel: &AtomicBool) -> Vec<WirelessReading> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for device in devices
        .iter()
        .filter(|d| d.vid == 0x1532 && d.feature_len >= 90)
    {
        if cancel.load(Ordering::Relaxed) || !seen.insert(device.pid) {
            continue;
        }
        let Some(session) = HidSession::open(&device.path, true) else {
            continue;
        };
        let buffer_len = (device.feature_len as usize).max(RAZER_BUFFER_LEN);
        for transaction_id in [0x1Fu8, 0x3F, 0xFF, 0x7F, 0x00] {
            if cancel.load(Ordering::Relaxed) {
                break;
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
            let mut charging = false;
            let mut charge_request = razer_build_command(
                RAZER_CMD_CHARGING,
                transaction_id,
                buffer_len >= RAZER_BUFFER_LEN,
            );
            charge_request.resize(buffer_len, 0);
            if session.set_feature(&charge_request) {
                unsafe { Sleep(15) };
                let mut charge_response = vec![0u8; buffer_len];
                if session.get_feature(&mut charge_response) {
                    charging = razer_parse_reply(&charge_response, RAZER_CMD_CHARGING)
                        .is_some_and(|value| value != 0);
                }
            }
            let name = if device.product.trim().is_empty() {
                "Razer Wireless".to_owned()
            } else {
                device.product.trim().to_owned()
            };
            result.push(reading(
                device.id("razer"),
                name,
                razer_level_01(raw_level),
                true,
                charging,
                BatterySource::RazerHid,
            ));
            break;
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
        if budget.expired(cancel) || !seen.insert((device.vid, device.pid)) {
            continue;
        }
        let Some(protocol) = ss_protocol_for_pid(device.pid) else {
            continue;
        };
        if device.vid != 0x1038
            || !matches!(device.usage_page, 0xFFC0 | 0xFF00)
            || !matches!(device.input_len, 64 | 65)
            || !matches!(device.output_len, 64 | 65)
        {
            continue;
        }
        let Some(session) = HidSession::open(&device.path, false) else {
            continue;
        };
        let out_len = (device.output_len.max(64)) as usize;
        let in_len = (device.input_len.max(64)) as usize;
        let request = ss_build_request(protocol, out_len, out_len == 65);
        let mut response = vec![0u8; in_len];
        let mut parsed = session
            .transact(
                &request,
                &mut response,
                250,
                |packet| ss_valid_packet(protocol, packet),
                cancel,
            )
            .and_then(|len| ss_parse(protocol, &response[..len]));
        if parsed.is_none() && out_len == 64 {
            let framed = ss_build_request(protocol, 65, true);
            let mut retry = vec![0u8; in_len];
            parsed = session
                .transact(
                    &framed,
                    &mut retry,
                    250,
                    |packet| ss_valid_packet(protocol, packet),
                    cancel,
                )
                .and_then(|len| ss_parse(protocol, &retry[..len]));
        }
        let Some(parsed) = parsed else { continue };
        let name = if device.product.trim().is_empty() {
            ss_default_name(device.pid).to_owned()
        } else {
            device.product.trim().to_owned()
        };
        if !parsed.connected {
            continue;
        }
        // Yedek pil (spare) bu API'de taşınmadığı için yalnızca ana pil raporlanır.
        result.push(reading(
            device.id("steelseries"),
            name,
            parsed.level,
            parsed.connected,
            parsed.charging,
            BatterySource::SteelSeriesHid,
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

fn reg_dword(key: Handle, name: &str) -> Option<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let mut kind = 0u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            wide(name).as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            (&mut value as *mut u32).cast::<u8>(),
            &mut size,
        )
    };
    if status == 0 && kind == REG_DWORD && size >= 4 {
        Some(value)
    } else {
        None
    }
}

fn reg_string(key: Handle, name: &str) -> Option<String> {
    let mut buffer = vec![0u8; 1024];
    let mut size = buffer.len() as u32;
    let mut kind = 0u32;
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
    if status == 0 && kind == REG_SZ {
        let text = utf16_from_bytes(&buffer[..size.min(buffer.len() as u32) as usize]);
        if text.is_empty() { None } else { Some(text) }
    } else {
        None
    }
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
/// Returns an empty vector when WinRT/Bluetooth is unavailable; the caller then
/// falls back to the cached registry values below.
fn ble_gatt_readings(cancel: &AtomicBool) -> Vec<WirelessReading> {
    use windows::Devices::Bluetooth::BluetoothLEDevice;
    use windows::Devices::Enumeration::DeviceInformation;
    use windows::Storage::Streams::DataReader;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    use windows::core::GUID;

    const BATTERY_SERVICE: GUID = GUID::from_u128(0x0000180f_0000_1000_8000_00805f9b34fb);
    const BATTERY_LEVEL: GUID = GUID::from_u128(0x00002a19_0000_1000_8000_00805f9b34fb);

    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() && hr.0 != 0x8001_0106u32 as i32 {
        return Vec::new();
    }
    let initialized = hr.is_ok();

    let result = (|| -> windows::core::Result<Vec<WirelessReading>> {
        let selector = BluetoothLEDevice::GetDeviceSelector()?;
        let devices = DeviceInformation::FindAllAsyncAqsFilter(&selector)?.get()?;
        let mut readings = Vec::new();
        for info in devices {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let device_id = info.Id()?;
            let name = info.Name()?.to_string_lossy();
            let read = (|| -> windows::core::Result<Option<u8>> {
                let device = BluetoothLEDevice::FromIdAsync(&device_id)?.get()?;
                let services = device.GetGattServicesAsync()?.get()?;
                for service in services.Services()? {
                    if service.Uuid()? != BATTERY_SERVICE {
                        continue;
                    }
                    let characteristics = service.GetCharacteristicsAsync()?.get()?;
                    for characteristic in characteristics.Characteristics()? {
                        if characteristic.Uuid()? != BATTERY_LEVEL {
                            continue;
                        }
                        let value = characteristic.ReadValueAsync()?.get()?;
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
                let id = format!(
                    "ble-gatt:{:08X}",
                    fnv1a_u32(device_id.to_string_lossy().as_bytes())
                );
                readings.push(reading(
                    id,
                    name,
                    percent_01(percent as i32),
                    true,
                    false,
                    BatterySource::BluetoothGatt,
                ));
            }
        }
        Ok(readings)
    })();
    if initialized {
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
    result.unwrap_or_default()
}

fn probe_ble_gatt(_devices: &[HidDevice], cancel: &AtomicBool) -> Vec<WirelessReading> {
    let mut result = ble_gatt_readings(cancel);
    if !result.is_empty() {
        return result;
    }
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
// Sağlayıcı 7-12: vendor HID pil capacity (gerçek HID usage değerleri)
// ---------------------------------------------------------------------------

fn probe_vendor_hid(
    devices: &[HidDevice],
    cancel: &AtomicBool,
    source: BatterySource,
) -> Vec<WirelessReading> {
    let filter = |device: &HidDevice| match source {
        BatterySource::AsusRogHid => device.vid == 0x0B05,
        BatterySource::CorsairHid => device.vid == 0x1B1C,
        BatterySource::HyperXHid => device.vid == 0x03F0,
        BatterySource::CompxAtkVxe => device.vid == 0x1EA7 || device.vid == 0x04E8,
        BatterySource::GamepadHid => {
            device.usage_page == 0x0001 && matches!(device.usage, 0x0004 | 0x0005)
        }
        BatterySource::Keyboard24gHid => {
            let name = device.product.to_ascii_lowercase();
            (name.contains("2.4g") || name.contains("2,4g"))
                && (name.contains("keyboard") || name.contains("klavye"))
        }
        _ => false,
    };
    probe_standard_hid(devices, cancel, source, filter)
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

fn cached_snapshot() -> Vec<WirelessReading> {
    let now = Instant::now();
    let mut entries = Vec::new();
    let mut guard = lock_cache();
    let mut expired = Vec::new();
    for (key, cached) in guard.iter_mut() {
        if now.saturating_duration_since(cached.sampled_at) > STALE_WINDOW {
            expired.push(key.clone());
            continue;
        }
        let mut item = cached.reading.clone();
        item.stale = true;
        entries.push(item);
    }
    for key in expired {
        guard.remove(&key);
    }
    drop(guard);
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
        let should_insert = deduplicated
            .get(&key)
            .is_none_or(|existing| prefer(&item, existing));
        if should_insert {
            deduplicated.insert(key, item);
        }
    }
    let active: HashSet<String> = deduplicated.keys().cloned().collect();
    let mut output = Vec::new();
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
    let mut expired = Vec::new();
    for (key, cached) in guard.iter() {
        if now.saturating_duration_since(cached.sampled_at) > STALE_WINDOW {
            expired.push(key.clone());
            continue;
        }
        if active.contains(key) {
            continue;
        }
        let mut item = cached.reading.clone();
        item.stale = true;
        output.push(item);
    }
    for key in expired {
        guard.remove(&key);
    }
    drop(guard);
    sort_readings(&mut output);
    output
}

fn run_probe(
    source: BatterySource,
    devices: &[HidDevice],
    cancel: &AtomicBool,
) -> Vec<WirelessReading> {
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
        | BatterySource::Keyboard24gHid => probe_vendor_hid(devices, cancel, source),
    }
}

/// Tüm kablosuz pil okumalarını toplar. Engelleyici değildir: her sağlayıcı ayrı
/// iş parçacığında 1.2 sn'lik bütçeyle çalışır, zaman aşanlar atlanır.
pub fn read_all() -> Vec<WirelessReading> {
    let gate = POLL_GATE.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = gate.try_lock() else {
        return cached_snapshot();
    };
    let devices = Arc::new(enumerate_hid_devices());
    let sources = [
        BatterySource::SteelSeriesHid,
        BatterySource::LogitechHidPlusPlus,
        BatterySource::RazerHid,
        BatterySource::XInput,
        BatterySource::StandardHid,
        BatterySource::BluetoothGatt,
        BatterySource::AsusRogHid,
        BatterySource::CorsairHid,
        BatterySource::GamepadHid,
        BatterySource::HyperXHid,
        BatterySource::CompxAtkVxe,
        BatterySource::Keyboard24gHid,
    ];
    let (sender, receiver) = mpsc::channel::<Vec<WirelessReading>>();
    let mut cancellations = Vec::with_capacity(sources.len());
    for source in sources {
        let sender = sender.clone();
        let devices = Arc::clone(&devices);
        let cancel = Arc::new(AtomicBool::new(false));
        cancellations.push(Arc::clone(&cancel));
        let _ = std::thread::Builder::new()
            .name(format!("ot-wb-{source:?}"))
            .spawn(move || {
                let result =
                    catch_unwind(AssertUnwindSafe(|| run_probe(source, &devices, &cancel)))
                        .unwrap_or_default();
                let _ = sender.send(result);
            });
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
    finalize(fresh)
}

// ---------------------------------------------------------------------------
// Donanım gerektirmeyen saf unit testler
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
