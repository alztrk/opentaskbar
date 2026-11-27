//! Wi-Fi SSID discovery and connection through the native Windows WLAN API.
//!
//! This is the platform half of the WS-11 / Fjord Wi-Fi flyout. It intentionally
//! uses hand-written FFI against `wlanapi.dll` so the workstream does not need to
//! touch the lead-owned `Cargo.toml` feature list.
//!
//! * [`scan_networks`] triggers a scan on every WLAN interface and waits at most
//!   [`SCAN_TIMEOUT`] (4 seconds) for the `wlan_notification_acm_scan_complete`
//!   notification before reading the BSS list.
//! * SSIDs are decoded as UTF-8 when possible and fall back to a stable hex form.
//! * [`WifiSecurity`] classifies Open / WPA / WPA2 / WPA3 from 802.11 information
//!   elements; [`build_profile_xml`] emits the matching WLAN profile XML.

use std::ffi::c_void;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::core::GUID;

/// Maximum time a single scan is allowed to take.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(4);
const ERROR_SUCCESS: u32 = 0;
const WLAN_NOTIFICATION_SOURCE_ACM: u32 = 8;
const WLAN_NOTIFICATION_ACM_SCAN_COMPLETE: u32 = 7;
const WLAN_INTF_OPCODE_CURRENT_CONNECTION: i32 = 7;
const WLAN_INTERFACE_STATE_CONNECTED: i32 = 1;
const DOT11_BSS_TYPE_INFRASTRUCTURE: i32 = 1;
const WLAN_CONNECTION_MODE_PROFILE: i32 = 0;
const DOT11_CAPABILITY_PRIVACY: u16 = 0x0010;

// 802.11 information element identifiers and AKM suite selectors.
const IE_RSN: u8 = 48;
const IE_VENDOR_SPECIFIC: u8 = 221;
const WPA_OUI: [u8; 3] = [0x00, 0x50, 0xF4];
const WPA_OUI_TYPE: u8 = 0x01;
const RSN_OUI: [u8; 3] = [0x00, 0x0F, 0xAC];
const AKM_SAE: u8 = 0x08; // 00-0F-AC:8
const AKM_OWE: u8 = 0x06; // 00-0F-AC:6
const AKM_PSK: u8 = 0x02; // 00-0F-AC:2

/// Security class of a discovered network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WifiSecurity {
    Open,
    Wpa,
    Wpa2,
    Wpa3,
}

impl WifiSecurity {
    /// WLAN profile `authentication` value.
    pub fn authentication(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Wpa => "WPAPSK",
            Self::Wpa2 => "WPA2PSK",
            Self::Wpa3 => "WPA3-SAE",
        }
    }

    /// WLAN profile `encryption` value.
    pub fn encryption(self) -> &'static str {
        match self {
            Self::Open => "none",
            Self::Wpa => "TKIP",
            Self::Wpa2 | Self::Wpa3 => "AES",
        }
    }

    /// Whether a password is required to connect.
    pub fn requires_password(self) -> bool {
        self != Self::Open
    }
}

/// Coarse signal-quality bucket for UI glyphs and colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SignalClass {
    Excellent,
    Good,
    Fair,
    Weak,
}

/// Maps a 0..100 signal percentage into a UI bucket.
pub fn signal_class(percent: u8) -> SignalClass {
    match percent {
        70..=100 => SignalClass::Excellent,
        45..=69 => SignalClass::Good,
        20..=44 => SignalClass::Fair,
        _ => SignalClass::Weak,
    }
}

/// A scan result consumed by the Fjord / WS-11 Wi-Fi flyout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    /// Human-readable SSID: UTF-8 when valid, otherwise `hex:...`.
    pub ssid: String,
    /// Link quality reported by the WLAN API, clamped to 0..100.
    pub signal_percent: u8,
    pub security: WifiSecurity,
    pub signal_class: SignalClass,
    /// True when this SSID is the one the interface is currently associated with.
    pub is_connected: bool,
}

impl WifiNetwork {
    pub fn new(
        ssid: String,
        signal_percent: u8,
        security: WifiSecurity,
        is_connected: bool,
    ) -> Self {
        let signal_percent = signal_percent.min(100);
        Self {
            ssid,
            signal_percent,
            security,
            signal_class: signal_class(signal_percent),
            is_connected,
        }
    }
}

/// Decodes a raw SSID. Invalid UTF-8 is rendered as a stable colon-separated hex
/// string so hidden/binary SSIDs stay addressable by the connect flow.
pub fn decode_ssid(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.is_empty() => text.to_owned(),
        _ if bytes.is_empty() => String::new(),
        _ => {
            let mut out = String::with_capacity(bytes.len() * 3 + 4);
            out.push_str("hex:");
            for (index, byte) in bytes.iter().enumerate() {
                if index > 0 {
                    out.push(':');
                }
                out.push_str(&format!("{byte:02X}"));
            }
            out
        }
    }
}

/// Classifies a network from the privacy capability bit and raw IE bytes.
///
/// RSN/OWE/SAKKeM AKM suites identify WPA3, RSN PSK identifies WPA2, the
/// vendor-specific WPA IE identifies WPA, and an open capability bit means Open.
pub fn security_from_ies(privacy: bool, ies: &[u8]) -> WifiSecurity {
    let mut offset = 0usize;
    let mut has_wpa = false;
    let mut has_rsn = false;
    while offset + 2 <= ies.len() {
        let id = ies[offset];
        let len = ies[offset + 1] as usize;
        let start = offset + 2;
        let end = start.saturating_add(len).min(ies.len());
        if end <= ies.len() {
            let body = &ies[start..end];
            if id == IE_RSN {
                has_rsn = true;
                if rsn_is_wpa3(body) {
                    return WifiSecurity::Wpa3;
                }
            } else if id == IE_VENDOR_SPECIFIC
                && body.len() >= 4
                && body[..3] == WPA_OUI
                && body[3] == WPA_OUI_TYPE
            {
                has_wpa = true;
            }
        }
        if len == 0 {
            offset = start;
        } else {
            offset = end;
        }
    }
    if has_rsn {
        WifiSecurity::Wpa2
    } else if has_wpa {
        WifiSecurity::Wpa
    } else if privacy {
        // Legacy WEP has no profile equivalent in the four-class contract; the
        // closest authenticated profile is WPA2 so the user can still be prompted.
        WifiSecurity::Wpa2
    } else {
        WifiSecurity::Open
    }
}

fn rsn_is_wpa3(body: &[u8]) -> bool {
    // RSN: version(2) + group cipher(4) + pairwise count(2) + pairwise suites...
    // + AKM count(2) + AKM suites(4 each). Locate the AKM count defensively.
    if body.len() < 10 {
        return false;
    }
    let mut at = 2 + 4; // version + group suite
    let pairwise_count = u16::from_le_bytes([body[at], body[at + 1]]) as usize;
    at += 2;
    at = at.saturating_add(pairwise_count.saturating_mul(4));
    if at + 2 > body.len() {
        return false;
    }
    let akm_count = u16::from_le_bytes([body[at], body[at + 1]]) as usize;
    at += 2;
    for _ in 0..akm_count {
        if at + 4 > body.len() {
            break;
        }
        let oui = &body[at..at + 3];
        let selector = body[at + 3];
        if oui == RSN_OUI && (selector == AKM_SAE || selector == AKM_OWE) {
            return true;
        }
        at += 4;
    }
    false
}

fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Builds a WLAN profile XML document for the requested security class.
pub fn build_profile_xml(ssid: &str, password: &str, security: WifiSecurity) -> String {
    let name = xml_escape(ssid);
    let hex = ssid
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<String>();
    let auth = security.authentication();
    let encryption = security.encryption();
    let shared_key = if security.requires_password() {
        format!(
            "<sharedKey><keyType>passPhrase</keyType><protected>false</protected>\
             <keyMaterial>{}</keyMaterial></sharedKey>",
            xml_escape(password)
        )
    } else {
        String::new()
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
         <name>{name}</name><SSIDConfig><SSID><hex>{hex}</hex></SSID></SSIDConfig>\
         <connectionType>ESS</connectionType><connectionMode>auto</connectionMode>\
         <MSM><security><authEncryption>\
         <authentication>{auth}</authentication><encryption>{encryption}</encryption>\
         <useOneX>false</useOneX></authEncryption>{shared_key}</security></MSM>\
         </WLANProfile>"
    )
}

// ---------------------------------------------------------------------------
// Native WLAN API declarations (wlanapi.dll)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Dot11Ssid {
    length: u32,
    ssid: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WlanRateSet {
    length: u32,
    rates: [u16; 126],
}

impl Default for WlanRateSet {
    fn default() -> Self {
        Self {
            length: 0,
            rates: [0; 126],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WlanBssEntry {
    ssid: Dot11Ssid,
    phy_id: u32,
    bssid: [u8; 6],
    bss_type: i32,
    phy_type: i32,
    rssi: i32,
    link_quality: u32,
    in_reg_domain: u8,
    beacon_period: u16,
    timestamp: u64,
    host_timestamp: u64,
    capabilities: u16,
    channel_frequency: u32,
    rate_set: WlanRateSet,
    ie_offset: u32,
    ie_size: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WlanBssList {
    total_size: u32,
    number_of_items: u32,
    entries: [WlanBssEntry; 1],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WlanInterfaceInfo {
    guid: GUID,
    description: [u16; 256],
    state: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WlanInterfaceInfoList {
    number_of_items: u32,
    index: u32,
    interfaces: [WlanInterfaceInfo; 1],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WlanAssociationAttributes {
    ssid: Dot11Ssid,
    bss_type: i32,
    bssid: [u8; 6],
    phy_type: i32,
    phy_index: u32,
    signal_quality: u32,
    rx_rate: u32,
    tx_rate: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WlanSecurityAttributes {
    security_enabled: i32,
    onex_enabled: i32,
    auth_algorithm: i32,
    cipher_algorithm: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WlanConnectionAttributes {
    state: i32,
    mode: i32,
    profile_name: [u16; 256],
    association: WlanAssociationAttributes,
    security: WlanSecurityAttributes,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WlanConnectionParameters {
    mode: i32,
    profile: *const u16,
    ssid: *mut Dot11Ssid,
    bssid_list: *mut c_void,
    bss_type: i32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct L2NotificationData {
    source: u32,
    code: u32,
    guid: GUID,
    data_size: u32,
    data: *mut c_void,
}

struct ScanSignal {
    completed: Mutex<Vec<[u8; 16]>>,
    cv: Condvar,
}

impl ScanSignal {
    const fn new() -> Self {
        Self {
            completed: Mutex::new(Vec::new()),
            cv: Condvar::new(),
        }
    }
}

static SCAN_SIGNAL: ScanSignal = ScanSignal::new();

unsafe extern "system" fn on_wlan_notification(data: *mut L2NotificationData, _ctx: *mut c_void) {
    if data.is_null() {
        return;
    }
    // SAFETY: the callback owns the pointer for the duration of the call and
    // wlanapi serialises notifications on a single thread.
    let notification = unsafe { &*data };
    if notification.code != WLAN_NOTIFICATION_ACM_SCAN_COMPLETE {
        return;
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(
            &notification.guid as *const GUID as *const u8,
            std::mem::size_of::<GUID>(),
        )
    };
    let mut key = [0u8; 16];
    key.copy_from_slice(bytes);
    if let Ok(mut completed) = SCAN_SIGNAL.completed.lock() {
        if !completed.contains(&key) {
            completed.push(key);
        }
        SCAN_SIGNAL.cv.notify_all();
    }
}

#[cfg(windows)]
#[link(name = "wlanapi")]
unsafe extern "system" {
    fn WlanOpenHandle(
        client_version: u32,
        reserved: *const c_void,
        negotiated_version: *mut u32,
        client_handle: *mut *mut c_void,
    ) -> u32;
    fn WlanCloseHandle(client_handle: *mut c_void, reserved: *const c_void) -> u32;
    fn WlanEnumInterfaces(
        client_handle: *mut c_void,
        reserved: *const c_void,
        interface_list: *mut *mut WlanInterfaceInfoList,
    ) -> u32;
    fn WlanFreeMemory(memory: *mut c_void);
    fn WlanRegisterNotification(
        client_handle: *mut c_void,
        notification_source: u32,
        ignore_duplicates: i32,
        callback: Option<unsafe extern "system" fn(*mut L2NotificationData, *mut c_void)>,
        context: *mut c_void,
        reserved: *const c_void,
        previous_source: *mut u32,
    ) -> u32;
    fn WlanScan(
        client_handle: *mut c_void,
        interface_guid: *const GUID,
        ssid: *const Dot11Ssid,
        ie_data: *const c_void,
        reserved: *const c_void,
    ) -> u32;
    fn WlanGetNetworkBssList(
        client_handle: *mut c_void,
        interface_guid: *const GUID,
        ssid: *const Dot11Ssid,
        bss_type: i32,
        security_enabled: i32,
        reserved: *const c_void,
        bss_list: *mut *mut WlanBssList,
    ) -> u32;
    fn WlanQueryInterface(
        client_handle: *mut c_void,
        interface_guid: *const GUID,
        opcode: i32,
        reserved: *const c_void,
        data_size: *mut u32,
        data: *mut *mut c_void,
        value_type: *mut i32,
    ) -> u32;
    fn WlanSetProfile(
        client_handle: *mut c_void,
        interface_guid: *const GUID,
        flags: u32,
        profile_xml: *const u16,
        all_users: i32,
        reserved: *const c_void,
        reason_code: *mut u32,
    ) -> u32;
    fn WlanConnect(
        client_handle: *mut c_void,
        interface_guid: *const GUID,
        parameters: *const WlanConnectionParameters,
        reserved: *const c_void,
    ) -> u32;
    fn WlanDisconnect(
        client_handle: *mut c_void,
        interface_guid: *const GUID,
        reserved: *const c_void,
    ) -> u32;
}

/// RAII wrapper around a WLAN client handle.
#[cfg(windows)]
struct ClientHandle(*mut c_void);

#[cfg(windows)]
impl ClientHandle {
    fn open() -> Option<Self> {
        let mut negotiated = 0u32;
        let mut handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: all out-pointers are valid, reserved is null.
        let status = unsafe { WlanOpenHandle(2, std::ptr::null(), &mut negotiated, &mut handle) };
        (status == ERROR_SUCCESS && !handle.is_null()).then_some(Self(handle))
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0
    }
}

#[cfg(windows)]
impl Drop for ClientHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle is owned exclusively by this wrapper.
            unsafe {
                WlanCloseHandle(self.0, std::ptr::null());
            }
        }
    }
}

#[cfg(windows)]
fn guid_key(guid: &GUID) -> [u8; 16] {
    // SAFETY: GUID is a repr(C) 16-byte POD.
    unsafe {
        let bytes = std::slice::from_raw_parts(guid as *const GUID as *const u8, 16);
        let mut key = [0u8; 16];
        key.copy_from_slice(bytes);
        key
    }
}

/// Snapshot of the WLAN interfaces, freed together with the WLAN memory blob.
#[cfg(windows)]
struct Interfaces {
    list: *mut WlanInterfaceInfoList,
}

#[cfg(windows)]
impl Interfaces {
    fn enumerate(client: &ClientHandle) -> Option<Self> {
        let mut list: *mut WlanInterfaceInfoList = std::ptr::null_mut();
        // SAFETY: out-pointer is valid; the returned blob is owned by `Self`.
        let status = unsafe { WlanEnumInterfaces(client.as_ptr(), std::ptr::null(), &mut list) };
        (status == ERROR_SUCCESS && !list.is_null()).then_some(Self { list })
    }

    fn slice(&self) -> &[WlanInterfaceInfo] {
        // SAFETY: the WLAN API allocates `number_of_items` contiguous entries.
        unsafe {
            let list = &*self.list;
            let count = list.number_of_items as usize;
            std::slice::from_raw_parts(list.interfaces.as_ptr(), count)
        }
    }
}

#[cfg(windows)]
impl Drop for Interfaces {
    fn drop(&mut self) {
        if !self.list.is_null() {
            // SAFETY: memory was allocated by WlanEnumInterfaces for this wrapper.
            unsafe { WlanFreeMemory(self.list.cast()) };
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn string_from_wide(units: &[u16]) -> String {
    let used = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    String::from_utf16_lossy(&units[..used])
}

#[cfg(windows)]
fn connected_ssid(client: &ClientHandle, guid: &GUID) -> String {
    let mut size = 0u32;
    let mut data: *mut c_void = std::ptr::null_mut();
    let mut value_type = 0i32;
    // SAFETY: out-pointers are valid; the returned buffer is freed below.
    let status = unsafe {
        WlanQueryInterface(
            client.as_ptr(),
            guid,
            WLAN_INTF_OPCODE_CURRENT_CONNECTION,
            std::ptr::null(),
            &mut size,
            &mut data,
            &mut value_type,
        )
    };
    if status != ERROR_SUCCESS || data.is_null() {
        return String::new();
    }
    // SAFETY: a successful query returned a WLAN_CONNECTION_ATTRIBUTES blob.
    let attributes = unsafe { &*(data as *const WlanConnectionAttributes) };
    let ssid = if attributes.state == WLAN_INTERFACE_STATE_CONNECTED {
        let ssid = &attributes.association.ssid;
        decode_ssid(&ssid.ssid[..(ssid.length as usize).min(32)])
    } else {
        String::new()
    };
    // SAFETY: buffer allocated by WlanQueryInterface.
    unsafe { WlanFreeMemory(data) };
    ssid
}

#[cfg(windows)]
fn wait_for_scan(guid: &GUID) {
    let key = guid_key(guid);
    if let Ok(mut completed) = SCAN_SIGNAL.completed.lock() {
        completed.retain(|entry| entry != &key);
    }
    let deadline = Instant::now() + SCAN_TIMEOUT;
    let mut completed = match SCAN_SIGNAL.completed.lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    while !completed.contains(&key) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match SCAN_SIGNAL.cv.wait_timeout(completed, remaining) {
            Ok((guard, _)) => completed = guard,
            Err(_) => return,
        }
    }
}

/// Reads the BSS list of one interface after a completed (or timed-out) scan.
#[cfg(windows)]
fn networks_on_interface(
    client: &ClientHandle,
    guid: &GUID,
    connected_ssid: &str,
) -> Vec<WifiNetwork> {
    let mut list: *mut WlanBssList = std::ptr::null_mut();
    // SAFETY: `security_enabled = 0` requests both open and secured networks.
    let status = unsafe {
        WlanGetNetworkBssList(
            client.as_ptr(),
            guid,
            std::ptr::null(),
            DOT11_BSS_TYPE_INFRASTRUCTURE,
            0,
            std::ptr::null(),
            &mut list,
        )
    };
    if status != ERROR_SUCCESS || list.is_null() {
        return Vec::new();
    }
    // SAFETY: the WLAN API owns this blob until WlanFreeMemory below.
    unsafe {
        let header = &*list;
        let total = header.total_size as usize;
        let count = (header.number_of_items as usize).min(256);
        let mut result = Vec::with_capacity(count);
        for index in 0..count {
            let entry = &header.entries[index];
            let ssid_len = (entry.ssid.length as usize).min(32);
            let ssid = decode_ssid(&entry.ssid.ssid[..ssid_len]);
            if ssid.is_empty() {
                continue;
            }
            let mut ies: &[u8] = &[];
            let ie_start = entry.ie_offset as usize;
            let ie_len = entry.ie_size as usize;
            let base = entry as *const WlanBssEntry as usize;
            let list_base = list as usize;
            if ie_start <= total
                && ie_len <= total - ie_start
                && base >= list_base
                && ie_start + ie_len <= total.saturating_sub(base - list_base)
            {
                ies = std::slice::from_raw_parts(base.wrapping_add(ie_start) as *const u8, ie_len);
            }
            let security =
                security_from_ies(entry.capabilities & DOT11_CAPABILITY_PRIVACY != 0, ies);
            result.push(WifiNetwork::new(
                ssid.clone(),
                entry.link_quality.min(100) as u8,
                security,
                ssid.eq_ignore_ascii_case(connected_ssid),
            ));
        }
        WlanFreeMemory(list.cast());
        result
    }
}

/// Scans all WLAN interfaces and returns the discovered SSIDs.
///
/// Each interface scan waits for the ACM scan-complete notification at most
/// [`SCAN_TIMEOUT`] (4 seconds); a stalled driver never blocks the caller longer.
pub fn scan_networks() -> Vec<WifiNetwork> {
    #[cfg(windows)]
    {
        let Some(client) = ClientHandle::open() else {
            return Vec::new();
        };
        let Some(interfaces) = Interfaces::enumerate(&client) else {
            return Vec::new();
        };
        // Register once for the whole batch so the callback can signal whichever
        // interface finishes its scan.
        // SAFETY: null context/reserved, callback has a static lifetime.
        unsafe {
            WlanRegisterNotification(
                client.as_ptr(),
                WLAN_NOTIFICATION_SOURCE_ACM,
                0,
                Some(on_wlan_notification),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
            );
        }
        let mut result = Vec::new();
        let snapshot = interfaces.slice().to_vec();
        for interface in &snapshot {
            // SAFETY: GUID comes from the enumerated interface list.
            unsafe {
                WlanScan(
                    client.as_ptr(),
                    &interface.guid,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                );
            }
            wait_for_scan(&interface.guid);
            let connected = connected_ssid(&client, &interface.guid);
            let mut found = networks_on_interface(&client, &interface.guid, &connected);
            // The associated SSID can disappear from the BSS list while roaming;
            // keep it visible so the flyout can show the current connection.
            if !connected.is_empty() && !found.iter().any(|network| network.ssid == connected) {
                found.push(WifiNetwork::new(connected, 100, WifiSecurity::Wpa2, true));
            }
            result.extend(found);
        }
        // SAFETY: unregister our callback before the client handle is closed.
        unsafe {
            WlanRegisterNotification(
                client.as_ptr(),
                0,
                0,
                None,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
            );
        }
        result.sort_by(|a, b| {
            b.is_connected
                .cmp(&a.is_connected)
                .then_with(|| b.signal_percent.cmp(&a.signal_percent))
                .then_with(|| {
                    a.ssid
                        .to_ascii_lowercase()
                        .cmp(&b.ssid.to_ascii_lowercase())
                })
        });
        result.dedup_by(|a, b| a.ssid.eq_ignore_ascii_case(&b.ssid));
        result
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Creates (or refreshes) a WLAN profile for `ssid` and connects to it.
///
/// The security class is derived from the last scan; an unknown SSID defaults to
/// WPA2 when a password is supplied, otherwise to Open.
pub fn connect(ssid: &str, password: &str) -> bool {
    #[cfg(windows)]
    {
        if ssid.trim().is_empty() {
            return false;
        }
        let security = scan_networks()
            .into_iter()
            .find(|network| network.ssid.eq_ignore_ascii_case(ssid))
            .map(|network| network.security)
            .unwrap_or(if password.is_empty() {
                WifiSecurity::Open
            } else {
                WifiSecurity::Wpa2
            });
        if security.requires_password() && password.is_empty() {
            return false;
        }
        let Some(client) = ClientHandle::open() else {
            return false;
        };
        let Some(interfaces) = Interfaces::enumerate(&client) else {
            return false;
        };
        let profile = wide(&build_profile_xml(ssid, password, security));
        let name = wide(ssid);
        let mut connected = false;
        for interface in interfaces.slice() {
            let mut reason = 0u32;
            // SAFETY: UTF-16 profile is null-terminated and borrowed for the call.
            let profile_status = unsafe {
                WlanSetProfile(
                    client.as_ptr(),
                    &interface.guid,
                    0,
                    profile.as_ptr(),
                    0,
                    std::ptr::null(),
                    &mut reason,
                )
            };
            if profile_status != ERROR_SUCCESS {
                continue;
            }
            let parameters = WlanConnectionParameters {
                mode: WLAN_CONNECTION_MODE_PROFILE,
                profile: name.as_ptr(),
                ssid: std::ptr::null_mut(),
                bssid_list: std::ptr::null_mut(),
                bss_type: DOT11_BSS_TYPE_INFRASTRUCTURE,
                flags: 0,
            };
            // SAFETY: all pointers are valid for the duration of the call.
            connected = unsafe {
                WlanConnect(
                    client.as_ptr(),
                    &interface.guid,
                    &parameters,
                    std::ptr::null(),
                )
            } == ERROR_SUCCESS;
            if connected {
                break;
            }
        }
        connected
    }
    #[cfg(not(windows))]
    {
        let _ = (ssid, password);
        false
    }
}

/// Disconnects every WLAN interface. Returns true when at least one succeeded.
pub fn disconnect() -> bool {
    #[cfg(windows)]
    {
        let Some(client) = ClientHandle::open() else {
            return false;
        };
        let Some(interfaces) = Interfaces::enumerate(&client) else {
            return false;
        };
        let mut disconnected = false;
        for interface in interfaces.slice() {
            // SAFETY: GUID is borrowed from the enumerated list.
            disconnected |=
                unsafe { WlanDisconnect(client.as_ptr(), &interface.guid, std::ptr::null()) }
                    == ERROR_SUCCESS;
        }
        disconnected
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssid_decodes_utf8_or_hex() {
        assert_eq!(decode_ssid(b"My Wifi"), "My Wifi");
        assert_eq!(decode_ssid(&[0x00, 0xFF, 0x41]), "hex:00:FF:41");
        assert_eq!(decode_ssid(b""), "");
    }

    #[test]
    fn signal_bucket_boundaries() {
        assert_eq!(signal_class(0), SignalClass::Weak);
        assert_eq!(signal_class(19), SignalClass::Weak);
        assert_eq!(signal_class(20), SignalClass::Fair);
        assert_eq!(signal_class(44), SignalClass::Fair);
        assert_eq!(signal_class(45), SignalClass::Good);
        assert_eq!(signal_class(69), SignalClass::Good);
        assert_eq!(signal_class(70), SignalClass::Excellent);
        assert_eq!(signal_class(100), SignalClass::Excellent);
    }

    fn rsn(akm: u8) -> Vec<u8> {
        // version, group cipher, one TKIP pairwise suite, one AKM suite.
        let mut body = vec![0x01, 0x00];
        body.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);
        body.extend_from_slice(&[0x01, 0x00]);
        body.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x02]);
        body.extend_from_slice(&[0x01, 0x00]);
        body.extend_from_slice(&RSN_OUI);
        body.push(akm);
        body
    }

    fn ie(id: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![id, body.len() as u8];
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn security_decision_from_ies() {
        assert_eq!(security_from_ies(false, &[]), WifiSecurity::Open);
        assert_eq!(security_from_ies(true, &[]), WifiSecurity::Wpa2);
        let wpa = ie(
            IE_VENDOR_SPECIFIC,
            &[WPA_OUI[0], WPA_OUI[1], WPA_OUI[2], WPA_OUI_TYPE, 0, 0],
        );
        assert_eq!(security_from_ies(true, &wpa), WifiSecurity::Wpa);
        let wpa2 = ie(IE_RSN, &rsn(AKM_PSK));
        assert_eq!(security_from_ies(true, &wpa2), WifiSecurity::Wpa2);
        let wpa3 = ie(IE_RSN, &rsn(AKM_SAE));
        assert_eq!(security_from_ies(true, &wpa3), WifiSecurity::Wpa3);
        let owe = ie(IE_RSN, &rsn(AKM_OWE));
        assert_eq!(security_from_ies(true, &owe), WifiSecurity::Wpa3);
        // Truncated RSN must not panic or be misclassified as WPA3.
        assert_eq!(
            security_from_ies(true, &[IE_RSN, 2, 1, 0]),
            WifiSecurity::Wpa2
        );
    }

    #[test]
    fn profile_xml_covers_all_security_classes() {
        let open = build_profile_xml("Guest & <WiFi>", "", WifiSecurity::Open);
        assert!(open.contains("Guest &amp; &lt;WiFi&gt;"));
        assert!(open.contains("<authentication>open</authentication>"));
        assert!(open.contains("<encryption>none</encryption>"));
        assert!(!open.contains("<sharedKey>"));
        for (security, auth, cipher) in [
            (WifiSecurity::Wpa, "WPAPSK", "TKIP"),
            (WifiSecurity::Wpa2, "WPA2PSK", "AES"),
            (WifiSecurity::Wpa3, "WPA3-SAE", "AES"),
        ] {
            let xml = build_profile_xml("Home", "p&<'\" ass", security);
            assert!(xml.contains(&format!("<authentication>{auth}</authentication>")));
            assert!(xml.contains(&format!("<encryption>{cipher}</encryption>")));
            assert!(xml.contains("<keyMaterial>p&amp;&lt;&apos;&quot; ass</keyMaterial>"));
        }
        assert!(WifiSecurity::Open.authentication() == "open");
    }

    #[test]
    fn wifi_network_normalizes_signal_and_class() {
        let network = WifiNetwork::new("Cafe".into(), 250, WifiSecurity::Wpa2, true);
        assert_eq!(network.signal_percent, 100);
        assert_eq!(network.signal_class, SignalClass::Excellent);
        assert!(network.is_connected);
    }
}
